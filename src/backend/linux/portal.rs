use std::collections::HashMap;
use std::fs::File;
use std::os::fd::OwnedFd as StdOwnedFd;
use std::path::PathBuf;

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedFd, Value};

pub(super) struct FileTransfer {
    pub key: Vec<u8>,
    pub backend: &'static str,
}

pub(super) fn start_file_transfer(paths: &[PathBuf]) -> Result<FileTransfer, String> {
    let conn = Connection::session().map_err(|err| err.to_string())?;
    let (proxy, backend) = file_transfer_proxy(&conn)?;

    let mut start_options = HashMap::new();
    start_options.insert("autostop", Value::Bool(true));
    let key: String = proxy
        .call("StartTransfer", &(start_options))
        .map_err(|err| err.to_string())?;

    let mut fds = Vec::with_capacity(paths.len());
    for path in paths {
        let file = File::open(path).map_err(|err| format!("{}: {err}", path.display()))?;
        let fd: StdOwnedFd = file.into();
        fds.push(OwnedFd::from(fd));
    }
    let add_options: HashMap<&str, Value<'_>> = HashMap::new();
    let _: () = proxy
        .call("AddFiles", &(&key, fds, add_options))
        .map_err(|err| err.to_string())?;

    Ok(FileTransfer {
        key: key.into_bytes(),
        backend,
    })
}

fn file_transfer_proxy(conn: &Connection) -> Result<(Proxy<'_>, &'static str), String> {
    let documents = Proxy::new(
        conn,
        "org.freedesktop.portal.Documents",
        "/org/freedesktop/portal/documents",
        "org.freedesktop.portal.FileTransfer",
    )
    .map_err(|err| err.to_string());
    if let Ok(proxy) = documents {
        return Ok((proxy, "Documents"));
    }

    Proxy::new(
        conn,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.FileTransfer",
    )
    .map(|proxy| (proxy, "Desktop"))
    .map_err(|err| err.to_string())
}
