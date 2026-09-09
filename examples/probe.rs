//! Development helper: prints what FlatBak sees of the host's Flatpak state.
//!
//! Run with `cargo run --example probe`. Useful for checking the host-escape
//! detection when FlatBak itself runs inside a container or a sandbox.

fn main() {
    let flatpak = flatbak::flatpak::Flatpak::detect();
    println!("command : {}", flatpak.command_line());
    match flatpak.version() {
        Ok(version) => println!("version : {version}"),
        Err(error) => println!("version : unavailable ({error:#})"),
    }

    match flatpak.list_apps() {
        Ok(apps) => {
            println!("apps    : {}", apps.len());
            for app in &apps {
                let data = flatbak::appdata::inspect(&app.id);
                println!(
                    "  {:<38} {:<10} {:<8} {:<7} data={:>9} cache={:>9}",
                    app.id,
                    app.origin,
                    app.branch,
                    app.installation.as_str(),
                    flatbak::util::format_size(data.total_bytes),
                    flatbak::util::format_size(data.cache_bytes),
                );
            }
            let ids: Vec<String> = apps.iter().map(|app| app.id.clone()).collect();
            println!("orphaned data: {:?}", flatbak::appdata::orphaned_data(&ids));
        }
        Err(error) => println!("apps    : failed ({error:#})"),
    }

    match flatpak.list_remotes() {
        Ok(remotes) => {
            println!("remotes : {}", remotes.len());
            for remote in &remotes {
                println!(
                    "  {:<12} {:<8} {}",
                    remote.name,
                    remote.installation.as_str(),
                    remote.url
                );
            }
        }
        Err(error) => println!("remotes : failed ({error:#})"),
    }
}
