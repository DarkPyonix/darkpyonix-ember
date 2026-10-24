//! `ember-shim --dir <dir> --id <session>`: holds one session's agent so it outlives the ember
//! server (SPEC FR-S8). Reads `<dir>/<id>.spec.json`; see `ember_server::session_shim`.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |flag: &str| args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone());
    let (Some(dir), Some(id)) = (get("--dir"), get("--id")) else {
        eprintln!("usage: ember-shim --dir <dir> --id <session>");
        std::process::exit(2);
    };
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("runtime");
    if let Err(e) = rt.block_on(ember_server::session_shim::run_shim(std::path::Path::new(&dir), &id)) {
        eprintln!("ember-shim: {e}");
        std::process::exit(1);
    }
}
