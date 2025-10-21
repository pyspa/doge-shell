use dsh_types::Context;
use std::os::unix::io::FromRawFd;
use tokio::{fs, io};

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Redirect {
    StdoutOutput(String),
    StdoutAppend(String),
    StderrOutput(String),
    StderrAppend(String),
    StdouterrOutput(String),
    StdouterrAppend(String),
    Input(String),
}

impl Redirect {
    pub(crate) fn process(&self, ctx: &mut Context) {
        match self {
            Redirect::StdoutOutput(out)
            | Redirect::StderrOutput(out)
            | Redirect::StdouterrOutput(out) => {
                let infile = ctx.infile;
                let file = out.to_string();
                // spawn and io copy
                tokio::spawn(async move {
                    // copy io
                    let mut reader = unsafe { fs::File::from_raw_fd(infile) };
                    match fs::File::create(&file).await {
                        Ok(mut writer) => {
                            if let Err(e) = io::copy(&mut reader, &mut writer).await {
                                tracing::error!("Failed to copy to file {}: {}", file, e);
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to create file {}: {}", file, e);
                        }
                    }
                });
            }

            Redirect::StdoutAppend(out)
            | Redirect::StderrAppend(out)
            | Redirect::StdouterrAppend(out) => {
                let infile = ctx.infile;
                let file = out.to_string();
                // spawn and io copy
                tokio::spawn(async move {
                    // copy io
                    let mut reader = unsafe { fs::File::from_raw_fd(infile) };
                    match fs::OpenOptions::new()
                        .write(true)
                        .append(true)
                        .open(&file)
                        .await
                    {
                        Ok(mut writer) => {
                            if let Err(e) = io::copy(&mut reader, &mut writer).await {
                                tracing::error!("Failed to append to file {}: {}", file, e);
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to open file {} for append: {}", file, e);
                        }
                    }
                });
            }
            Redirect::Input(_) => {}
        }
    }
}
