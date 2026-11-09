//! The small part of COM/WMI this tool needs on Windows: queries, one
//! method call with an embedded object, and typed property access.

use anyhow::{Context, Result, anyhow, bail};
use std::mem::ManuallyDrop;
use std::sync::Once;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
    CoInitializeSecurity, CoSetProxyBlanket, CoTaskMemFree, EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL,
    RPC_C_AUTHN_LEVEL_DEFAULT, RPC_C_IMP_LEVEL_IMPERSONATE,
};
use windows::Win32::System::Rpc::{RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE};
use windows::Win32::System::Variant::{
    InitVariantFromBuffer, VARIANT, VT_BOOL, VT_EMPTY, VT_I4, VT_NULL, VT_UNKNOWN, VariantClear,
    VariantGetElementCount, VariantToBoolean, VariantToBuffer, VariantToInt32,
    VariantToStringAlloc, VariantToUInt32,
};
use windows::Win32::System::Wmi::{
    IWbemClassObject, IWbemLocator, IWbemServices, WBEM_FLAG_FORWARD_ONLY,
    WBEM_FLAG_RETURN_IMMEDIATELY, WBEM_FLAG_RETURN_WBEM_COMPLETE, WBEM_INFINITE, WbemLocator,
};
use windows::core::{BSTR, IUnknown, Interface, PCWSTR};

thread_local! {
    /// COM stays initialized for the thread's lifetime once used.
    static COM: windows::core::Result<()> = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok() };
}
static SECURITY: Once = Once::new();

fn init_com() -> Result<()> {
    COM.with(|com| match com {
        Ok(()) => Ok(()),
        // RPC_E_CHANGED_MODE: the thread already joined an STA, also usable
        Err(e) if e.code().0 as u32 == 0x8001_0106 => Ok(()),
        Err(e) => Err(anyhow!("COM initialization failed: {e}")),
    })?;
    SECURITY.call_once(|| unsafe {
        // Fails harmlessly (RPC_E_TOO_LATE) if the process already set it.
        let _ = CoInitializeSecurity(
            None,
            -1,
            None,
            None,
            RPC_C_AUTHN_LEVEL_DEFAULT,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            None,
            EOAC_NONE,
            None,
        );
    });
    Ok(())
}

/// WBEM_E_ACCESS_DENIED and E_ACCESSDENIED.
const ACCESS_DENIED: [u32; 2] = [0x8004_1003, 0x8007_0005];
/// WBEM_E_INVALID_CLASS, e.g. hpqBIntM on a PC that is not an HP.
const INVALID_CLASS: u32 = 0x8004_1010;

/// `what` failed with `e`, with a hint for the common causes.
fn failed(e: windows::core::Error, what: String) -> anyhow::Error {
    let hint = match e.code().0 as u32 {
        code if ACCESS_DENIED.contains(&code) => " (run as administrator)",
        INVALID_CLASS => " (class not present on this PC)",
        _ => "",
    };
    anyhow::Error::new(e).context(format!("{what}{hint}"))
}

/// A connected WMI namespace such as `root\wmi`.
pub struct Namespace(IWbemServices);

impl Namespace {
    pub fn connect(namespace: &str) -> Result<Self> {
        init_com()?;
        unsafe {
            let locator: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)
                .context("WMI locator")?;
            let empty = BSTR::new();
            let services = locator
                .ConnectServer(
                    &BSTR::from(namespace),
                    &empty,
                    &empty,
                    &empty,
                    0,
                    &empty,
                    None,
                )
                .map_err(|e| failed(e, format!("connect to WMI namespace {namespace}")))?;
            CoSetProxyBlanket(
                &services,
                RPC_C_AUTHN_WINNT,
                RPC_C_AUTHZ_NONE,
                PCWSTR::null(),
                RPC_C_AUTHN_LEVEL_CALL,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
            )
            .context("WMI proxy security")?;
            Ok(Namespace(services))
        }
    }

    /// All objects a WQL query returns.
    pub fn query(&self, wql: &str) -> Result<Vec<Object>> {
        let mut found = Vec::new();
        unsafe {
            let rows = self
                .0
                .ExecQuery(
                    &BSTR::from("WQL"),
                    &BSTR::from(wql),
                    WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
                    None,
                )
                .map_err(|e| failed(e, wql.to_string()))?;
            loop {
                let mut row = [None];
                let mut returned = 0;
                rows.Next(WBEM_INFINITE, &mut row, &mut returned)
                    .ok()
                    .map_err(|e| failed(e, wql.to_string()))?;
                match row {
                    [Some(object)] if returned > 0 => found.push(Object(object)),
                    _ => break,
                }
            }
        }
        Ok(found)
    }

    /// The first instance of `class`, if any.
    pub fn first(&self, class: &str) -> Result<Option<Object>> {
        Ok(self
            .query(&format!("SELECT * FROM {class}"))?
            .into_iter()
            .next())
    }

    /// A class definition, for spawning instances and method parameters.
    pub fn class(&self, name: &str) -> Result<Object> {
        let mut object = None;
        unsafe {
            self.0
                .GetObject(
                    &BSTR::from(name),
                    WBEM_FLAG_RETURN_WBEM_COMPLETE,
                    None,
                    Some(&mut object),
                    None,
                )
                .with_context(|| format!("WMI class {name}"))?;
        }
        object
            .map(Object)
            .ok_or_else(|| anyhow!("WMI class {name} missing"))
    }

    /// Runs `method` on the object at `path`; returns its out-parameters.
    pub fn exec_method(&self, path: &str, method: &str, input: &Object) -> Result<Object> {
        let mut output = None;
        unsafe {
            self.0
                .ExecMethod(
                    &BSTR::from(path),
                    &BSTR::from(method),
                    Default::default(),
                    None,
                    &input.0,
                    Some(&mut output),
                    None,
                )
                .map_err(|e| failed(e, format!("WMI method {method}")))?;
        }
        output
            .map(Object)
            .ok_or_else(|| anyhow!("WMI method {method} returned nothing"))
    }
}

/// A WMI class or instance.
pub struct Object(IWbemClassObject);

impl Object {
    pub fn spawn(&self) -> Result<Object> {
        unsafe {
            self.0
                .SpawnInstance(0)
                .map(Object)
                .context("spawn WMI instance")
        }
    }

    /// A fresh in-parameters instance for `method` of this class.
    pub fn method_input(&self, method: &str) -> Result<Object> {
        let mut input = None;
        let name = BSTR::from(method);
        unsafe {
            self.0
                .GetMethod(PCWSTR(name.as_ptr()), 0, &mut input, std::ptr::null_mut())
                .with_context(|| format!("WMI method {method} signature"))?;
        }
        input
            .map(Object)
            .ok_or_else(|| anyhow!("WMI method {method} takes no input"))?
            .spawn()
    }

    pub fn get(&self, name: &str) -> Result<Variant> {
        let mut value = Variant::empty();
        let wide = BSTR::from(name);
        unsafe {
            self.0
                .Get(PCWSTR(wide.as_ptr()), 0, &mut value.0, None, None)
                .with_context(|| format!("WMI property {name}"))?;
        }
        Ok(value)
    }

    pub fn put(&self, name: &str, value: &Variant) -> Result<()> {
        let wide = BSTR::from(name);
        unsafe {
            self.0
                .Put(PCWSTR(wide.as_ptr()), 0, &value.0, 0)
                .with_context(|| format!("set WMI property {name}"))
        }
    }

    /// `__RELPATH`, the object's address within its namespace, for
    /// `exec_method`.
    pub fn relative_path(&self) -> Result<String> {
        self.get("__RELPATH")?.string()
    }

    /// A property, or None if missing, null or not convertible.
    pub fn u32(&self, name: &str) -> Option<u32> {
        self.get(name).ok()?.u32().ok()
    }
    pub fn i32(&self, name: &str) -> Option<i32> {
        self.get(name).ok()?.i32().ok()
    }
    pub fn bool(&self, name: &str) -> Option<bool> {
        self.get(name).ok()?.bool().ok()
    }
    pub fn string(&self, name: &str) -> Option<String> {
        self.get(name).ok()?.string().ok()
    }
}

/// An owned VARIANT, cleared on drop.
pub struct Variant(VARIANT);

impl Drop for Variant {
    fn drop(&mut self) {
        unsafe {
            let _ = VariantClear(&mut self.0);
        }
    }
}

impl Variant {
    fn empty() -> Self {
        Variant(VARIANT::default())
    }

    fn vt(&self) -> u16 {
        unsafe { self.0.Anonymous.Anonymous.vt.0 }
    }

    fn is_null(&self) -> bool {
        self.vt() == VT_NULL.0 || self.vt() == VT_EMPTY.0
    }

    /// WMI wants CIM uint32 values as VT_I4.
    pub fn from_u32(value: u32) -> Self {
        let mut v = Self::empty();
        unsafe {
            let inner = &mut *v.0.Anonymous.Anonymous;
            inner.vt = VT_I4;
            inner.Anonymous.lVal = value as i32;
        }
        v
    }

    /// A `uint8[]` property value (VT_ARRAY | VT_UI1).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let len = u32::try_from(bytes.len())?;
        unsafe { Ok(Variant(InitVariantFromBuffer(bytes.as_ptr().cast(), len)?)) }
    }

    /// An embedded object property value.
    pub fn from_object(object: &Object) -> Result<Self> {
        let unknown: IUnknown = object.0.cast()?;
        let mut v = Self::empty();
        unsafe {
            let inner = &mut *v.0.Anonymous.Anonymous;
            inner.vt = VT_UNKNOWN;
            inner.Anonymous.punkVal = ManuallyDrop::new(Some(unknown));
        }
        Ok(v)
    }

    fn not_null(&self) -> Result<()> {
        if self.is_null() {
            bail!("value is null");
        }
        Ok(())
    }

    /// Fails for negative values, e.g. 0xFFFFFFFF ("unknown" in battery
    /// classes), which WMI hands over as a VT_I4 of -1.
    pub fn u32(&self) -> Result<u32> {
        self.not_null()?;
        unsafe { Ok(VariantToUInt32(&self.0)?) }
    }

    pub fn i32(&self) -> Result<i32> {
        self.not_null()?;
        unsafe { Ok(VariantToInt32(&self.0)?) }
    }

    pub fn bool(&self) -> Result<bool> {
        self.not_null()?;
        if self.vt() != VT_BOOL.0 {
            return Ok(self.u32()? != 0);
        }
        unsafe { Ok(VariantToBoolean(&self.0)?.as_bool()) }
    }

    pub fn string(&self) -> Result<String> {
        self.not_null()?;
        unsafe {
            let text = VariantToStringAlloc(&self.0)?;
            let result = text.to_string();
            CoTaskMemFree(Some(text.0.cast()));
            Ok(result?)
        }
    }

    pub fn bytes(&self) -> Result<Vec<u8>> {
        self.not_null()?;
        unsafe {
            let mut bytes = vec![0u8; VariantGetElementCount(&self.0) as usize];
            if !bytes.is_empty() {
                VariantToBuffer(&self.0, bytes.as_mut_ptr().cast(), bytes.len() as u32)?;
            }
            Ok(bytes)
        }
    }

    pub fn object(&self) -> Result<Object> {
        self.not_null()?;
        if self.vt() != VT_UNKNOWN.0 {
            bail!("value is not an object (VARTYPE {})", self.vt());
        }
        unsafe {
            let unknown = self
                .0
                .Anonymous
                .Anonymous
                .Anonymous
                .punkVal
                .as_ref()
                .ok_or_else(|| anyhow!("null object"))?;
            Ok(Object(unknown.cast()?))
        }
    }
}
