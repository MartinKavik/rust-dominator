use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;
use web_sys::{self, console};

#[wasm_bindgen(start)]
pub async fn main_js() {
    spawn_local(async { console::log_1(&"From task!".into()) });

    console::log_1(&"Open the window!".into());
    
    web_sys::window()
        .unwrap_throw()
        .open_with_url("http://example.com")
        .unwrap_throw()
        .unwrap_throw()
        .close()
        .unwrap_throw();
    
    console::log_1(&"Main End!".into());
}
