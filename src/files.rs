use anyhow::{anyhow, Context as _, Result};
use async_std::{
    fs, io,
    path::{Path, PathBuf},
    pin::Pin,
    prelude::*,
    task::{block_on, Context, Poll},
};
use rust_embed::RustEmbed;
use tempfile::TempDir;
use url::Url;

const FILES_FOLDER_RELATIVE: &'static str = "files";
const FILES_URL_BASE: &'static str = "https://ttsmagic.cards/files/";
// const STATIC_URL_BASE: &'static str = "https://ttsmagic.cards/static/";

#[derive(RustEmbed)]
#[folder = "static/"]
pub struct StaticFiles;

// impl StaticFiles {
//     pub fn get_url(name: &'static str) -> Result<Url> {
//         match Self::get(name) {
//             None => Err(anyhow!("Invalid static file reference: {}", name)),
//             Some(_) => {
//                 let base = Url::parse(STATIC_URL_BASE).unwrap();
//                 Ok(base.join(name)?)
//             }
//         }
//     }
// }

#[derive(Debug)]
pub struct MediaFile {
    rel_path: PathBuf,
    root: PathBuf,
    // file: Option<fs::File>,
}

impl Clone for MediaFile {
    fn clone(&self) -> Self {
        MediaFile {
            rel_path: self.rel_path.clone(),
            root: self.root.clone(),
            // file: None,
        }
    }
}

impl MediaFile {
    pub fn create<P: AsRef<Path>, R: AsRef<Path>>(root: R, name: P) -> Result<WritableMediaFile> {
        let root = root.as_ref().join(FILES_FOLDER_RELATIVE);
        WritableMediaFile::new(root, name)
    }

    pub async fn path_exists<P: AsRef<Path>, R: AsRef<Path>>(root: R, name: P) -> Option<PathBuf> {
        let rel_path = name.as_ref().to_owned();
        let root = root.as_ref().join(FILES_FOLDER_RELATIVE);
        let full_filename = root.join(&rel_path);
        debug!(
            "Looking for existence of path {} (from rel path {})",
            full_filename.to_string_lossy(),
            rel_path.to_string_lossy()
        );
        if full_filename.is_file().await {
            Some(full_filename)
        } else {
            None
        }
    }

    // pub async fn get<P: AsRef<Path>, R: AsRef<Path>>(root: R, name: P) -> Result<Self> {
    //     let rel_path = name.as_ref().to_owned();
    //     let root = root.as_ref().join(FILES_FOLDER_RELATIVE);
    //     // let file = Some(fs::File::open(full_filename).await?);
    //     Ok(Self {
    //         rel_path,
    //         root,
    //         // file,
    //     })
    // }

    // pub async fn open(&self) -> Result<fs::File> {
    //     let full_filename = self.root.join(&self.rel_path);
    //     Ok(fs::File::open(full_filename).await?)
    // }

    pub fn path(&self) -> PathBuf {
        self.root.join(&self.rel_path)
    }

    pub fn url(&self) -> Result<Url> {
        let base = Url::parse(FILES_URL_BASE).unwrap();
        let path = self.rel_path.to_str().ok_or_else(|| {
            anyhow!(
                "MediaFile with rel path {:?} cannot be converted into a string safely",
                self.rel_path.to_string_lossy()
            )
        })?;
        Ok(base.join(path)?)
    }
}

pub struct WritableMediaFile {
    // TODO: this is a blocking API. Should investigate to see if there's an
    // async/await version of the tempfile package.
    rel_filename: PathBuf,
    root: PathBuf,
    temp_dir: TempDir,
    temp_file: Option<fs::File>,
}

// fn check_tmp() -> Result<()> {
//     use std::os::unix::fs::PermissionsExt;

//     let tmp_root = std::path::Path::new("/tmp");

//     debug!("Checking tmp directory");
//     let metadata = match std::fs::metadata(&tmp_root) {
//         Ok(m) => m,
//         Err(e) => {
//             warn!("Got error checking metadata on /tmp: {}", e);
//             std::fs::create_dir(&tmp_root)?;
//             debug!("Created /tmp directory");
//             std::fs::metadata(&tmp_root)?
//         }
//     };
//     let perms = metadata.permissions();
//     debug!("Got perms for /tmp: {:?}", perms);
//     if (perms.mode() & 0o7777) != 0o1777 {
//         warn!(
//             "/tmp mode is incorrect. Expected 1777, got {:04o}",
//             perms.mode()
//         );
//         std::fs::set_permissions(&tmp_root, std::fs::Permissions::from_mode(0o1777))?;
//     }

//     debug!("/tmp directory permissions are ok now");

//     Ok(())
// }

impl WritableMediaFile {
    fn new<N: AsRef<Path>>(root: PathBuf, name: N) -> Result<Self> {
        // if let Err(e) = check_tmp() {
        //     error!("Got error checking /tmp directory: {}", e);
        //     Err(e)?
        // }

        let rel_filename = name.as_ref().to_owned();
        if let None = rel_filename.file_name() {
            // This invariant is used in `Self::path`.
            return Err(anyhow!(
                "Media file name {} is invalid (no file name component)",
                rel_filename.to_string_lossy()
            ));
        }
        Ok(Self {
            rel_filename,
            root,
            temp_dir: tempfile::tempdir()?,
            temp_file: None,
        })
    }

    fn ensure_temp_file(&mut self) -> Result<&mut fs::File, io::Error> {
        let f = match self.temp_file.take() {
            Some(f) => f,
            None => {
                // Block here to simplify `Write` impl.
                block_on(
                    fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&self.path()),
                )?
            }
        };
        self.temp_file = Some(f);
        Ok(self.temp_file.as_mut().unwrap())
    }

    pub fn path(&self) -> PathBuf {
        // The expect here should be impossible because it's checked in `Self::new`.
        let basename = &self
            .rel_filename
            .file_name()
            .expect("WritableMediaFile::path, self.rel_filename has invalid base name");
        self.temp_dir.path().join(basename).into()
    }

    async fn finish_tempfile(mut self) -> Result<(PathBuf, PathBuf)> {
        let dest_path = self.root.join(&self.rel_filename);
        let temp_file_path = self.path();
        if !temp_file_path.is_file().await {
            return Err(anyhow!("Media file"));
        }
        if let Some(f) = self.temp_file.as_mut() {
            f.flush().await.context("Finalizing media file")?;
        }
        let directory = dest_path
            .parent()
            .ok_or_else(|| {
                anyhow!(
                    "Media file {} has no parent directory",
                    self.rel_filename.to_string_lossy()
                )
            })
            .context("Finalizing media file")?;

        debug!(
            "Creating directory for media file: {}",
            directory.to_string_lossy()
        );
        fs::create_dir_all(directory)
            .await
            .context("Finalizing media file")?;

        let mut rel_filename: PathBuf = self.rel_filename;
        let mut final_path: PathBuf = dest_path.clone();
        {
            let rel_filename_dir: PathBuf = rel_filename
                .parent()
                .ok_or_else(|| {
                    anyhow!(
                        "Media file {} has no parent directory",
                        rel_filename.to_string_lossy()
                    )
                })?
                .to_owned();
            let basename_no_ext: String = final_path
                .file_stem()
                .ok_or_else(|| {
                    anyhow!("Filename {} has no basename", final_path.to_string_lossy())
                })?
                .to_string_lossy()
                .into_owned();
            let ext: std::ffi::OsString = final_path
                .extension()
                .unwrap_or_else(|| std::ffi::OsStr::new(""))
                .to_owned();
            use rand::Rng;
            while final_path.is_file().await {
                let mut random_part = String::with_capacity(8);
                for _ in 0..8 {
                    random_part
                        .push(rand::thread_rng().gen_range('a' as u8, ('z' as u8) + 1) as char);
                }
                let new_basename = format!(
                    "{}_{}.{}",
                    basename_no_ext,
                    random_part,
                    ext.to_string_lossy()
                );
                debug!(
                    "{} is taken, so trying with basename {} instead",
                    final_path.to_string_lossy(),
                    new_basename,
                );
                final_path = directory.join(&new_basename);
                rel_filename = rel_filename_dir.join(&new_basename);
            }
        }

        debug!(
            "Renaming temp file to for media file: {}",
            final_path.to_string_lossy()
        );
        if let Err(_) = fs::rename(&temp_file_path, &final_path).await {
            debug!("Rename failed, copying instead");
            let start = chrono::Utc::now();
            fs::copy(&temp_file_path, &final_path)
                .await
                .context("Finalizing media file")?;
            let end = chrono::Utc::now();
            let duration = end - start;
            debug!(
                "Copied {} to {} in {}",
                temp_file_path.to_string_lossy(),
                final_path.to_string_lossy(),
                duration,
            );
        }

        Ok((final_path, rel_filename))
    }

    pub async fn close(self) -> Result<()> {
        let (_full_filename, _rel) = self.finish_tempfile().await?;
        Ok(())
    }

    pub async fn finalize(self) -> Result<MediaFile> {
        let root = self.root.clone();
        let (_full_filename, rel_path) = self.finish_tempfile().await?;
        Ok(MediaFile { root, rel_path })
    }
}

impl io::Write for WritableMediaFile {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        let f = self.ensure_temp_file()?;
        pin_mut!(f);
        f.poll_write(cx, buffer)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        let f = self.ensure_temp_file()?;
        pin_mut!(f);
        f.poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        let f = self.ensure_temp_file()?;
        pin_mut!(f);
        f.poll_close(cx)
    }
}
