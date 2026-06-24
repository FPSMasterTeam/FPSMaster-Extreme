//! Headless ground-truth check of the DLSS render path: upscale a constant mid-grey
//! frame and read back the output centre. Tells us whether DLSS produces a non-black
//! image from valid inputs — isolating "DLSS broken" from "renderer wiring broken".
//!
//! Run on an RTX + Vulkan box with the SDK set up + nvngx_dlss.dll next to the exe:
//!   cargo run -p recraft_render --example dlss_check --features dlss

#[cfg(feature = "dlss")]
fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    match recraft_render::dlss::run_selftest() {
        Ok(avg) => {
            println!("DLSS selftest: output centre avg = {avg:.4} (input was 0.5 grey)");
            if avg < 0.02 {
                println!("==> OUTPUT IS BLACK: DLSS produced no image from valid inputs.");
            } else {
                println!("==> OUTPUT IS NON-BLACK: the DLSS render path works in isolation.");
            }
        }
        Err(e) => println!("DLSS selftest FAILED: {e}"),
    }
}

#[cfg(not(feature = "dlss"))]
fn main() {
    println!("Build with --features dlss to run the DLSS selftest.");
}
