use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::snapshot::Snapshot;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Request {
    Snapshot {
        min_fraction: f32,
        #[serde(default)]
        focus: Vec<String>,
        #[serde(default)]
        open: Vec<String>,
    },
    Rescan { mount: Option<String> },
    Expand { path: String },
    Ping,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Response {
    Snapshot(Snapshot),
    Pong,
    Ok,
    Error(String),
}

pub fn socket_path() -> PathBuf {
    dirs::runtime_dir()
        .or_else(|| std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir)
        .join("spacemap.sock")
}

pub fn data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("spacemap")
}

pub fn write_msg(w: &mut impl Write, msg: &impl Serialize) -> Result<()> {
    serde_json::to_writer(&mut *w, msg)?;
    w.write_all(b"\n")?;
    w.flush()?;
    Ok(())
}

pub fn read_msg<T: for<'de> Deserialize<'de>>(r: &mut impl BufRead) -> Result<T> {
    let mut line = String::new();
    let n = r.read_line(&mut line)?;
    if n == 0 {
        anyhow::bail!("eof");
    }
    Ok(serde_json::from_str(line.trim_end())?)
}

pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Client {
    pub fn connect() -> Result<Self> {
        let stream = UnixStream::connect(socket_path()).context("connect spacemapd")?;
        stream.set_read_timeout(Some(Duration::from_secs(8)))?;
        stream.set_write_timeout(Some(Duration::from_secs(8)))?;
        let writer = stream.try_clone()?;
        Ok(Self {
            reader: BufReader::new(stream),
            writer,
        })
    }

    pub fn snapshot(
        &mut self,
        min_fraction: f32,
        focus: &[String],
        open: &[String],
    ) -> Result<Snapshot> {
        write_msg(
            &mut self.writer,
            &Request::Snapshot {
                min_fraction,
                focus: focus.to_vec(),
                open: open.to_vec(),
            },
        )?;
        match read_msg::<Response>(&mut self.reader)? {
            Response::Snapshot(s) => Ok(s),
            Response::Error(e) => anyhow::bail!(e),
            _ => anyhow::bail!("unexpected response"),
        }
    }

    pub fn rescan(&mut self, mount: Option<String>) -> Result<()> {
        write_msg(&mut self.writer, &Request::Rescan { mount })?;
        match read_msg::<Response>(&mut self.reader)? {
            Response::Ok => Ok(()),
            Response::Error(e) => anyhow::bail!(e),
            _ => anyhow::bail!("unexpected response"),
        }
    }

    pub fn ping(&mut self) -> Result<()> {
        write_msg(&mut self.writer, &Request::Ping)?;
        match read_msg::<Response>(&mut self.reader)? {
            Response::Pong => Ok(()),
            Response::Error(e) => anyhow::bail!(e),
            _ => anyhow::bail!("unexpected response"),
        }
    }

    pub fn expand(&mut self, path: String) -> Result<()> {
        write_msg(&mut self.writer, &Request::Expand { path })?;
        match read_msg::<Response>(&mut self.reader)? {
            Response::Ok => Ok(()),
            Response::Error(e) => anyhow::bail!(e),
            _ => anyhow::bail!("unexpected response"),
        }
    }
}
