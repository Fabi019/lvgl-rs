use crate::functions::CoreError;
use crate::Box;
use crate::{disp_drv_register, disp_get_default, get_str_act};
use crate::{Color, Obj};
use core::marker::PhantomData;
use core::ptr::NonNull;
use core::{ptr, result};
use embedded_graphics::drawable;
use embedded_graphics::prelude::*;

#[cfg(feature = "alloc")]
use parking_lot::Mutex;

#[cfg(feature = "alloc")]
use alloc::sync::Arc;

// TODO: Make this an external configuration
const REFRESH_BUFFER_LEN: usize = 2;
// Declare a buffer for the refresh rate
pub(crate) const BUF_SIZE: usize = lvgl_sys::LV_HOR_RES_MAX as usize * REFRESH_BUFFER_LEN;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum DisplayError {
    NotAvailable,
    FailedToRegister,
    NotRegistered,
}

type Result<T> = result::Result<T, DisplayError>;

#[cfg(feature = "alloc")]
pub type SharedNativeDisplay<T> = Arc<Mutex<T>>;

pub struct Display {
    pub(crate) disp: NonNull<lvgl_sys::lv_disp_t>,
}

impl Display {
    pub(crate) fn from_raw(disp: NonNull<lvgl_sys::lv_disp_t>) -> Self {
        Self { disp }
    }

    // pub fn register<T, C>(native_display: T) -> Result<Self>
    // where
    //     T: DrawTarget<C>,
    //     C: PixelColor + From<Color>,
    // {
    //     let mut display_diver = DisplayDriver::new(DisplayBuffer::new(), native_display);
    //     Ok(disp_drv_register(&mut display_diver)?)
    // }

    #[cfg(feature = "alloc")]
    pub fn register_shared<T, C>(shared_native_display: &SharedNativeDisplay<T>) -> Result<Self>
    where
        T: DrawTarget<C>,
        C: PixelColor + From<Color>,
    {
        let mut display_diver =
            DisplayDriver::new_shared(DisplayBuffer::new(), Arc::clone(shared_native_display));
        Ok(disp_drv_register(&mut display_diver)?)
    }

    pub fn get_str_act(&self) -> Result<Obj> {
        Ok(get_str_act(Some(&self))?)
    }
}

impl Default for Display {
    fn default() -> Self {
        disp_get_default().expect("LVGL must be initialized")
    }
}

#[derive(Copy, Clone)]
pub struct DefaultDisplay {}

impl DefaultDisplay {
    /// Gets the screen active of the default display.
    pub fn get_scr_act() -> Result<Obj> {
        Ok(get_str_act(None)?)
    }
}

pub struct DisplayBuffer {
    disp_buf: Box<lvgl_sys::lv_disp_buf_t>,
}

impl DisplayBuffer {
    pub fn new() -> Self {
        let disp_buf = {
            let mut disp_buf = Box::new(lvgl_sys::lv_disp_buf_t::default());
            let refresh_buffer = Box::new([lvgl_sys::lv_color_t::default(); BUF_SIZE]);
            unsafe {
                lvgl_sys::lv_disp_buf_init(
                    disp_buf.as_mut() as *mut _,
                    Box::into_raw(refresh_buffer) as *mut cty::c_void,
                    ptr::null_mut(),
                    lvgl_sys::LV_HOR_RES_MAX * REFRESH_BUFFER_LEN as u32,
                );
            }
            disp_buf
        };

        Self { disp_buf }
    }
}

pub struct DisplayDriver<T, C>
where
    T: DrawTarget<C>,
    C: PixelColor + From<Color>,
{
    pub(crate) disp_drv: Box<lvgl_sys::lv_disp_drv_t>,
    phantom_display: PhantomData<T>,
    phantom_color: PhantomData<C>,
}

impl<T, C> DisplayDriver<T, C>
where
    T: DrawTarget<C>,
    C: PixelColor + From<Color>,
{
    #[cfg(feature = "alloc")]
    pub fn new_shared(
        display_buffer: DisplayBuffer,
        shared_native_display: SharedNativeDisplay<T>,
    ) -> Self {
        let mut disp_drv = Box::new(lvgl_sys::lv_disp_drv_t::default());
        unsafe {
            lvgl_sys::lv_disp_drv_init(disp_drv.as_mut() as *mut _);
        }

        // We need to add to a `Box`, so it's copied to a memory location in the "heap" (LVGL statically allocated heap).
        disp_drv.buffer = Box::into_raw(display_buffer.disp_buf) as *mut lvgl_sys::lv_disp_buf_t;

        let native_display = Box::new(SharedDisplayUserData {
            display: shared_native_display,
            phantom: PhantomData,
        });
        disp_drv.user_data =
            Box::into_raw(native_display) as *mut _ as lvgl_sys::lv_disp_drv_user_data_t;

        // Sets trampoline pointer to the function implementation using the types (T, C) that
        // are used in this instance of `DisplayDriver`.
        disp_drv.flush_cb = Some(shared_disp_flush_trampoline::<T, C>);

        // We do not store any memory that can be accidentally deallocated by on the Rust side.
        Self {
            disp_drv,
            phantom_color: PhantomData,
            phantom_display: PhantomData,
        }
    }
}

#[cfg(feature = "alloc")]
pub(crate) struct SharedDisplayUserData<T, C>
where
    T: DrawTarget<C>,
    C: PixelColor + From<Color>,
{
    display: SharedNativeDisplay<T>,
    phantom: PhantomData<C>,
}

#[cfg(feature = "alloc")]
unsafe extern "C" fn shared_disp_flush_trampoline<T, C>(
    disp_drv: *mut lvgl_sys::lv_disp_drv_t,
    area: *const lvgl_sys::lv_area_t,
    color_p: *mut lvgl_sys::lv_color_t,
) where
    T: DrawTarget<C>,
    C: PixelColor + From<Color>,
{
    let display_driver = *disp_drv;
    if !display_driver.user_data.is_null() {
        let user_data = &mut *(display_driver.user_data as *mut SharedDisplayUserData<T, C>);
        let x1 = (*area).x1;
        let x2 = (*area).x2;
        let y1 = (*area).y1;
        let y2 = (*area).y2;

        let ys = y1..=y2;
        let xs = (x1..=x2).enumerate();
        let x_len = (x2 - x1 + 1) as usize;

        // We use iterators here to ensure that the Rust compiler can apply all possible
        // optimizations at compile time.
        let pixels = ys
            .enumerate()
            .map(|(iy, y)| {
                xs.clone().map(move |(ix, x)| {
                    let color_len = x_len * iy + ix;
                    let lv_color = unsafe { *color_p.add(color_len) };
                    let raw_color = Color::from_raw(lv_color);
                    drawable::Pixel::<C>(Point::new(x as i32, y as i32), raw_color.into())
                })
            })
            .flatten();

        let _ = user_data
            .display
            .try_lock()
            .map(move |mut display| display.draw_iter(pixels));
    }
}

impl From<CoreError> for DisplayError {
    fn from(err: CoreError) -> Self {
        use DisplayError::*;
        match err {
            CoreError::ResourceNotAvailable => NotAvailable,
            CoreError::OperationFailed => NotAvailable,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests;

    #[test]
    fn get_scr_act_return_display() {
        tests::initialize_test();
        let _screen = get_str_act(None).expect("We can get the active screen");
    }

    #[test]
    fn get_default_display() {
        tests::initialize_test();
        let display = Display::default();

        let _screen_direct = display
            .get_str_act()
            .expect("Return screen directly from the display instance");

        let _screen_default =
            DefaultDisplay::get_scr_act().expect("Return screen from the default display");
    }

    #[test]
    fn register_display_directly() -> Result<()> {
        tests::initialize_test();
        let display = Display::default();

        let _screen = display
            .get_str_act()
            .expect("Return screen directly from the display instance");

        Ok(())
    }
}
