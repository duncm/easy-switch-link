use anyhow::{Context, Result, ensure};
use easy_switch_core::{Request, Response};
use interprocess::local_socket::{
    ListenerOptions, Name,
    tokio::{Stream, prelude::*},
};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub type Envelope = (Request, tokio::sync::oneshot::Sender<Response>);
const LIMIT: usize = 256 * 1024;

fn name(paths: &crate::storage::Paths) -> Result<Name<'static>> {
    #[cfg(windows)]
    {
        use interprocess::local_socket::{GenericNamespaced, ToNsName};
        let _ = paths;
        Ok(
            format!("easy-switch-link-{}", easy_switch_platform::user_id()?)
                .to_ns_name::<GenericNamespaced>()?,
        )
    }
    #[cfg(unix)]
    {
        use interprocess::local_socket::{GenericFilePath, ToFsName};
        Ok(paths
            .root
            .join("control.sock")
            .to_fs_name::<GenericFilePath>()?
            .into_owned())
    }
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?)
}

pub fn call(paths: &crate::storage::Paths, request: Request) -> Result<Response> {
    runtime()?.block_on(async {
        tokio::time::timeout(Duration::from_secs(10), async {
            let stream = Stream::connect(name(paths)?)
                .await
                .context("Monitor is not running; launch Easy-Switch Link first")?;
            let (mut read, mut write) = stream.split();
            let bytes = serde_json::to_vec(&request)?;
            write_frame(&mut write, &bytes).await?;
            let reply = read_frame(&mut read).await?;
            Ok(serde_json::from_slice(&reply)?)
        })
        .await
        .context("Monitor did not respond within 10 seconds")?
    })
}

async fn read_frame<R: tokio::io::AsyncRead + Unpin>(read: &mut R) -> Result<Vec<u8>> {
    let length = read.read_u32_le().await? as usize;
    ensure!(length > 0 && length <= LIMIT, "IPC message too large");
    let mut bytes = vec![0; length];
    read.read_exact(&mut bytes).await?;
    Ok(bytes)
}
async fn write_frame<W: tokio::io::AsyncWrite + Unpin>(write: &mut W, bytes: &[u8]) -> Result<()> {
    ensure!(bytes.len() <= LIMIT, "IPC response too large");
    write.write_u32_le(bytes.len() as u32).await?;
    write.write_all(bytes).await?;
    write.flush().await?;
    Ok(())
}

pub fn serve(paths: crate::storage::Paths, tx: crossbeam_channel::Sender<Envelope>) -> Result<()> {
    let (ready_tx, ready_rx) = crossbeam_channel::bounded(1);
    std::thread::Builder::new()
        .name("local-control".into())
        .spawn(move || {
            let result = (|| -> Result<()> {
                runtime()?.block_on(async {
                    let options = ListenerOptions::new().name(name(&paths)?);
                    #[cfg(windows)]
                    let options = {
                        use interprocess::os::windows::{
                            local_socket::ListenerOptionsExt,
                            security_descriptor::SecurityDescriptor,
                        };
                        let sddl = widestring::U16CString::from_str(format!(
                            "D:P(A;;GA;;;{})",
                            easy_switch_platform::user_id()?
                        ))?;
                        options.security_descriptor(SecurityDescriptor::deserialize(&sddl)?)
                    };
                    #[cfg(unix)]
                    let options = {
                        use interprocess::os::unix::local_socket::ListenerOptionsExt;
                        options.mode(0o600).try_overwrite(true)
                    };
                    let listener = options.create_tokio()?;
                    let _ = ready_tx.send(Ok(()));
                    let slots = Arc::new(tokio::sync::Semaphore::new(8));
                    loop {
                        let conn = listener.accept().await?;
                        let Ok(permit) = slots.clone().try_acquire_owned() else {
                            continue;
                        };
                        let tx = tx.clone();
                        tokio::spawn(async move {
                            let _permit = permit;
                            let result = tokio::time::timeout(Duration::from_secs(8), async {
                                let (mut read, mut write) = conn.split();
                                let request: Request =
                                    serde_json::from_slice(&read_frame(&mut read).await?)?;
                                let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                                tx.try_send((request, reply_tx))
                                    .context("Control queue full")?;
                                let response = reply_rx.await?;
                                write_frame(&mut write, &serde_json::to_vec(&response)?).await
                            })
                            .await;
                            if let Ok(Err(error)) = result {
                                tracing::debug!("Control request: {error:#}");
                            }
                        });
                    }
                    #[allow(unreachable_code)]
                    Ok::<(), anyhow::Error>(())
                })
            })();
            if let Err(error) = result {
                let _ = ready_tx.send(Err(format!("{error:#}")));
            }
        })?;
    ready_rx.recv()?.map_err(anyhow::Error::msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reject_oversized_frame_before_allocating() {
        runtime().unwrap().block_on(async {
            let mut bytes = &0x7fff_ffffu32.to_le_bytes()[..];
            assert!(read_frame(&mut bytes).await.is_err());
        });
    }
}
