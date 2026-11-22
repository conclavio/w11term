//! The WinUI island shares one input HWND among its controls. Track its native
//! synchronous GettingFocus event, so the message hook can route keys without
//! consulting UI Automation or walking the visual tree on input.
use std::{ffi::c_void, ptr};
use windows::{
    Foundation::EventHandler,
    core::{
        AgileReference, HRESULT, HSTRING, IInspectable, IInspectable_Vtbl, Interface, Result,
        RuntimeName, RuntimeType, factory, imp,
    },
};

// Minimal projection of Microsoft.UI.Xaml.winmd from the Windows App SDK 2.4.0
// runtime deployed by windows-reactor-setup 0.100. Slots below are zero-based
// MethodDef ordinals *after* IInspectable. Unused entries preserve ABI offsets.
type Raw = *mut c_void;
type GetObject = unsafe extern "system" fn(Raw, *mut Raw) -> HRESULT;

imp::define_interface!(
    FocusManager,
    FocusManagerVtable,
    0xe73dce04_e23a_5fb3_96ab_7df04c51dff2
);
impl RuntimeName for FocusManager {
    const NAME: &'static str = "Microsoft.UI.Xaml.Input.FocusManager";
}
#[repr(C)]
pub struct FocusManagerVtable {
    base: IInspectable_Vtbl,
    unused: [usize; 4],
    add_getting_focus: unsafe extern "system" fn(Raw, Raw, *mut i64) -> HRESULT,
    remove_getting_focus: unsafe extern "system" fn(Raw, i64) -> HRESULT,
}

imp::define_interface!(
    FocusArgs,
    FocusArgsVtable,
    0x37fd3af0_bd3c_5bf5_a9cd_71a1e87af950
);
impl RuntimeType for FocusArgs {
    // EventHandler<T> takes the runtime *class* signature, with its default interface.
    const SIGNATURE: imp::ConstBuffer = imp::ConstBuffer::from_slice(
        b"rc(Microsoft.UI.Xaml.Input.GettingFocusEventArgs;{37fd3af0-bd3c-5bf5-a9cd-71a1e87af950})",
    );
}
#[repr(C)]
pub struct FocusArgsVtable {
    base: IInspectable_Vtbl,
    old: GetObject,
    new: GetObject,
    unused: [usize; 6],
    cancel: unsafe extern "system" fn(Raw, *mut bool) -> HRESULT,
}

imp::define_interface!(
    AutomationProperties,
    AutomationPropertiesVtable,
    0xb1e3e0f3_112f_5966_87dc_7862d4ad50e5
);
impl RuntimeName for AutomationProperties {
    const NAME: &'static str = "Microsoft.UI.Xaml.Automation.AutomationProperties";
}
#[repr(C)]
pub struct AutomationPropertiesVtable {
    base: IInspectable_Vtbl,
    unused: [usize; 7],
    get_automation_id: unsafe extern "system" fn(Raw, Raw, *mut HSTRING) -> HRESULT,
}

imp::define_interface!(
    VisualTreeHelper,
    VisualTreeHelperVtable,
    0x5aece43c_7651_5bb5_855c_2198496e455e
);
impl RuntimeName for VisualTreeHelper {
    const NAME: &'static str = "Microsoft.UI.Xaml.Media.VisualTreeHelper";
}
#[repr(C)]
pub struct VisualTreeHelperVtable {
    base: IInspectable_Vtbl,
    unused: [usize; 6],
    get_parent: unsafe extern "system" fn(Raw, Raw, *mut Raw) -> HRESULT,
}

// Both focus getters and GetParent return an owned, nullable DependencyObject.
// Store its original default-interface pointer; these methods take that same ABI.
fn object(get: impl FnOnce(*mut Raw) -> HRESULT) -> Result<Option<IInspectable>> {
    let mut raw = ptr::null_mut();
    get(&mut raw).ok()?;
    Ok((!raw.is_null()).then(|| unsafe { IInspectable::from_raw(raw) }))
}

fn ownership(
    args: &FocusArgs,
    tag: &HSTRING,
    properties: &AgileReference<AutomationProperties>,
    tree: &AgileReference<VisualTreeHelper>,
) -> Result<Option<bool>> {
    let mut cancelled = false;
    unsafe { (args.vtable().cancel)(args.as_raw(), &mut cancelled) }.ok()?;
    if cancelled {
        return Ok(None);
    }
    let properties = properties.resolve()?;
    let tree = tree.resolve()?;
    let inside = |get: GetObject| -> Result<bool> {
        let mut element = object(|out| unsafe { get(args.as_raw(), out) })?;
        while let Some(current) = element {
            let mut id = HSTRING::new();
            unsafe {
                (properties.vtable().get_automation_id)(
                    properties.as_raw(),
                    current.as_raw(),
                    &mut id,
                )
            }
            .ok()?;
            if id == *tag {
                return Ok(true);
            }
            element = object(|out| unsafe {
                (tree.vtable().get_parent)(tree.as_raw(), current.as_raw(), out)
            })?;
        }
        Ok(false)
    };
    if inside(args.vtable().new)? {
        Ok(Some(true))
    } else if inside(args.vtable().old)? {
        Ok(Some(false))
    } else {
        // A different pane or island changed focus.
        Ok(None)
    }
}

pub(super) struct Watch {
    manager: FocusManager,
    token: i64,
}

impl Watch {
    pub(super) fn new(tag: String, update: impl Fn(bool) + Send + 'static) -> Result<Self> {
        let manager = factory::<FocusManager, FocusManager>()?;
        let properties =
            AgileReference::new(&factory::<AutomationProperties, AutomationProperties>()?)?;
        let tree = AgileReference::new(&factory::<VisualTreeHelper, VisualTreeHelper>()?)?;
        let tag = HSTRING::from(tag);
        let handler = EventHandler::<FocusArgs>::new(move |_, args| {
            if let Some(args) = args.as_ref()
                && let Ok(Some(focused)) = ownership(args, &tag, &properties, &tree)
            {
                update(focused);
            }
            Ok(())
        });
        let mut token = 0;
        unsafe {
            (manager.vtable().add_getting_focus)(manager.as_raw(), handler.as_raw(), &mut token)
        }
        .ok()?;
        Ok(Self { manager, token })
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let _ = unsafe {
            (self.manager.vtable().remove_getting_focus)(self.manager.as_raw(), self.token)
        };
    }
}
