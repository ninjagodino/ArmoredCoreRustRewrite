//! Temporary: test which asset paths exist on a disc.
use acvd_formats::vfs;

fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let disc = if args.first().map(String::as_str) == Some("--disc") {
        let p = std::path::PathBuf::from(args.remove(1));
        args.remove(0);
        vfs::Disc::open(&p)?
    } else {
        vfs::Disc::open(&vfs::repo_root().join(vfs::X360_ISO))?
    };
    for path in args {
        match disc.size(&path) {
            Ok(n) => println!("{n:>10} {path}"),
            Err(_) => println!("{:<10} {path}", "missing"),
        }
    }
    Ok(())
}
