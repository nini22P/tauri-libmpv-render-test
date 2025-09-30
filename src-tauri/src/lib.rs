use glutin::context::NotCurrentGlContext;
use glutin::display::DisplayApiPreference;
use glutin::prelude::GlDisplay;
use glutin::surface::{GlSurface, WindowSurface};
use libmpv2::events::Event;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use std::ffi::{c_void, CString};
use std::sync::{mpsc, Arc};
use std::{num::NonZeroU32, thread};
use tauri::Manager;

use libmpv2::{
    render::{OpenGLInitParams, RenderContext, RenderParam, RenderParamApiType},
    Mpv,
};

pub trait GlWindow {
    fn build_surface_attributes(
        &self,
        builder: glutin::surface::SurfaceAttributesBuilder<WindowSurface>,
    ) -> Result<glutin::surface::SurfaceAttributes<WindowSurface>, raw_window_handle::HandleError>;
}

impl GlWindow for tauri::WebviewWindow {
    fn build_surface_attributes(
        &self,
        builder: glutin::surface::SurfaceAttributesBuilder<WindowSurface>,
    ) -> Result<glutin::surface::SurfaceAttributes<WindowSurface>, raw_window_handle::HandleError>
    {
        let (w, h) = self
            .inner_size()
            .unwrap()
            .non_zero()
            .expect("invalid zero inner size");
        let handle = self.window_handle()?.as_raw();
        Ok(builder.build(handle, w, h))
    }
}

trait NonZeroU32PhysicalSize {
    fn non_zero(self) -> Option<(NonZeroU32, NonZeroU32)>;
}

impl NonZeroU32PhysicalSize for winit::dpi::PhysicalSize<u32> {
    fn non_zero(self) -> Option<(NonZeroU32, NonZeroU32)> {
        let w = NonZeroU32::new(self.width)?;
        let h = NonZeroU32::new(self.height)?;
        Some((w, h))
    }
}

fn get_proc_address(display: &Arc<glutin::display::Display>, name: &str) -> *mut c_void {
    match CString::new(name) {
        Ok(c_str) => display.get_proc_address(&c_str) as *mut _,
        Err(_) => std::ptr::null_mut(),
    }
}

#[derive(Debug)]
enum MpvThreadEvent {
    Redraw,
    MpvEvents,
}

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let window = app.get_webview_window("main").unwrap();

            thread::spawn(move || {
                let raw_window_handle = window.window_handle().unwrap().as_raw();
                let raw_display_handle = window.display_handle().unwrap().as_raw();

                let display = Arc::new(unsafe {
                    let preference = DisplayApiPreference::WglThenEgl(Some(raw_window_handle));
                    glutin::display::Display::new(raw_display_handle, preference)
                        .expect("Failed to create glutin display")
                });

                let surface_attributes: glutin::surface::SurfaceAttributes<WindowSurface> =
                    window.build_surface_attributes(Default::default()).unwrap();
                let template = glutin::config::ConfigTemplateBuilder::new()
                    .compatible_with_native_window(raw_window_handle);

                let config = unsafe {
                    display
                        .find_configs(template.build())
                        .unwrap()
                        .next()
                        .expect("No suitable config found")
                };

                let surface = unsafe {
                    display
                        .create_window_surface(&config, &surface_attributes)
                        .expect("Failed to create window surface")
                };

                let context_attributes =
                    glutin::context::ContextAttributesBuilder::new().build(Some(raw_window_handle));

                let context = unsafe {
                    display
                        .create_context(&config, &context_attributes)
                        .expect("Failed to create context")
                };

                let current_context = context
                    .make_current(&surface)
                    .expect("Failed to make context current");

                let mut mpv = Mpv::with_initializer(|init| {
                    init.set_option("vo", "libmpv")?;
                    init.set_option("hwdec", "auto-safe")?;
                    Ok(())
                })
                .expect("Failed to create mpv instance with initializer");

                let mut render_context = RenderContext::new(
                    unsafe { mpv.ctx.as_mut() },
                    vec![
                        RenderParam::ApiType(RenderParamApiType::OpenGl),
                        RenderParam::InitParams(OpenGLInitParams {
                            get_proc_address,
                            ctx: display.clone(),
                        }),
                    ],
                )
                .expect("Failed creating render context");

                let (event_tx, event_rx) = mpsc::channel::<MpvThreadEvent>();

                let redraw_tx = event_tx.clone();
                render_context.set_update_callback(move || {
                    redraw_tx.send(MpvThreadEvent::Redraw).ok();
                });

                mpv.set_wakeup_callback(move || {
                    event_tx.send(MpvThreadEvent::MpvEvents).ok();
                });

                let video_path = "https://commondatastorage.googleapis.com/gtv-videos-bucket/sample/BigBuckBunny.mp4";
                mpv.command("loadfile", &[video_path, "replace"]).unwrap();

                for event in event_rx {
                    match event {
                        MpvThreadEvent::Redraw => {
                            let size = window.inner_size().unwrap();
                            // println!("Redrawing frame at size: {}x{}", size.width, size.height);

                            render_context
                                .render::<Arc<glutin::display::Display>>(
                                    0,
                                    size.width as _,
                                    size.height as _,
                                    true,
                                )
                                .expect("Failed to draw video frame");

                            surface
                                .swap_buffers(&current_context)
                                .expect("Failed to swap buffers");
                        }
                        MpvThreadEvent::MpvEvents => {
                            while let Some(mpv_event) = mpv.wait_event(0.0) {
                                match mpv_event {
                                    Ok(Event::EndFile(_)) => {
                                        println!("End of file detected. Exiting render thread.");
                                        return;
                                    }
                                    Ok(e) => {
                                        println!("Received MPV Event: {:?}", e);
                                    }
                                    Err(e) => {
                                        println!("MPV event error: {}", e);
                                        return;
                                    }
                                }
                            }
                        }
                    }
                }
            });

            Ok(())
        })
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![greet])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
