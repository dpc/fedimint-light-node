use std::io::{self, Read};
use std::path::Path;

/// Read bounded credentials from a private regular file in a caller-owned directory.
pub fn read(path: &Path) -> io::Result<(String, String)> {
    let metadata = path.symlink_metadata()?;
    if !metadata.is_file() || metadata.len() > 514 {
        return Err(io::Error::other(
            "RPC credentials must be a bounded regular file, not a symlink",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::other(
                "RPC credentials must not be accessible to group/others (chmod 600)",
            ));
        }
    }
    let mut text = String::new();
    std::fs::File::open(path)?
        .take(515)
        .read_to_string(&mut text)?;
    if text.len() > 514 {
        return Err(io::Error::other("RPC credentials too long"));
    }
    let text = text.strip_suffix('\n').unwrap_or(&text);
    let (username, password) = text
        .split_once(':')
        .ok_or_else(|| io::Error::other("RPC credentials require username:password"))?;
    Ok((username.to_owned(), password.to_owned()))
}
