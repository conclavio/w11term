//! WinUI's protected cursor setter, adapted from windows-rs PR #5414.
use std::{ffi::c_void, ptr};
use windows_core::{
    HRESULT, IInspectable, IInspectable_Vtbl, Interface, Result, RuntimeName, factory, imp,
};

type Raw = *mut c_void;

imp::define_interface!(
    UIElementProtected,
    UIElementProtectedVtable,
    0x8f69b9e9_1f00_5834_9bf1_a9257bed39f0
);
#[repr(C)]
pub struct UIElementProtectedVtable {
    base: IInspectable_Vtbl,
    get_cursor: usize,
    set_cursor: unsafe extern "system" fn(Raw, Raw) -> HRESULT,
}

imp::define_interface!(
    InputSystemCursorStatics,
    InputSystemCursorStaticsVtable,
    0xd3860bb6_698a_5814_aedd_c2fa8bba5a02
);
impl RuntimeName for InputSystemCursorStatics {
    const NAME: &'static str = "Microsoft.UI.Input.InputSystemCursor";
}
#[repr(C)]
pub struct InputSystemCursorStaticsVtable {
    base: IInspectable_Vtbl,
    create: unsafe extern "system" fn(Raw, i32, *mut Raw) -> HRESULT,
}

imp::define_interface!(
    InputCursor,
    IInspectable_Vtbl,
    0x359b15f9_19c2_5714_8432_75176826406b
);

pub(super) fn set(control: &super::bindings::Border, shape: Option<i32>) -> Result<()> {
    let element = control.cast::<UIElementProtected>()?;
    let cursor = shape.map(create).transpose()?;
    let raw = cursor.as_ref().map_or(ptr::null_mut(), Interface::as_raw);
    // SAFETY: raw is the borrowed IInputCursor interface, or null to inherit.
    unsafe { (element.vtable().set_cursor)(element.as_raw(), raw) }.ok()
}

fn create(shape: i32) -> Result<InputCursor> {
    let factory = factory::<InputSystemCursorStatics, InputSystemCursorStatics>()?;
    let mut raw = ptr::null_mut();
    // SAFETY: on success Create returns the owned default interface of InputSystemCursor.
    unsafe {
        (factory.vtable().create)(factory.as_raw(), shape, &mut raw).and_then(|| {
            imp::Type::from_abi(raw).and_then(|cursor: IInspectable| cursor.cast())
        })
    }
}
