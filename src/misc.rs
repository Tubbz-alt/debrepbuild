use std::ffi::CString;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use libc;
use md5;
use walkdir::{DirEntry, WalkDir};

pub fn walk_debs(path: &Path) -> Box<Iterator<Item = DirEntry>> {
    fn is_deb(entry: &DirEntry) -> bool {
        if entry.path().is_dir() {
            true
        } else {
            entry.file_name().to_str().map_or(false, |e| e.ends_with(".deb"))
        }
    }

    Box::new(WalkDir::new(path).into_iter().filter_entry(|e| is_deb(e)).flat_map(|e| e.ok()))
}

pub fn match_deb(entry: &DirEntry, packages: &[String]) -> Option<(String, usize)> {
    let path = entry.path();
    if path.is_dir() {
        return None
    }

    entry.file_name().to_str().and_then(|package| {
        let package = &package[..package.find('_').expect("debian package lacks _ character")];

        packages.iter().position(|x| x.as_str() == package)
            .and_then(|pos| path.to_str().map(|path| (path.to_owned(), pos)))
    })
}

pub fn unlink(link: &Path) -> io::Result<()> {
    CString::new(link.to_path_buf().into_os_string().into_vec())
        .map_err(|why| io::Error::new(io::ErrorKind::InvalidInput, format!("{}", why)))
        .and_then(|link| match unsafe { libc::unlink(link.as_ptr()) } {
            0 => Ok(()),
            _ => Err(io::Error::last_os_error())
        })
}

pub fn rsync(src: &Path, dst: &Path) -> io::Result<()> {
    eprintln!("rsyncing {} to {}", src.display(), dst.display());

    if src.is_dir() {
        fs::create_dir_all(src)?;
    }

    Command::new("rsync")
        .arg("-avz")
        .arg(src)
        .arg(dst)
        .status()
        .and_then(|x| if x.success() {
            Ok(())
        } else {
            Err(io::Error::new(io::ErrorKind::Other, "tar command failed"))
        })
}

pub fn md5_digest(file: File) -> io::Result<String> {
    let mut context = md5::Context::new();
    let data = &mut BufReader::new(file);
    loop {
        let read = {
            let buffer = data.fill_buf()?;
            if buffer.len() == 0 { break }
            context.consume(buffer);
            buffer.len()
        };

        data.consume(read);
    }

    Ok(format!("{:x}", context.compute()))
}

pub fn extract(src: &Path, dst: &Path) -> io::Result<()> {
    match src.file_name().and_then(|x| x.to_str()) {
        Some(filename) => {
            if filename.ends_with(".zip") {
                unzip(src, dst)
            } else if filename.ends_with(".tar.gz") || filename.ends_with(".tar.xz") {
                untar(src, dst)
            } else {
                unimplemented!()
            }
        }
        None => unimplemented!()
    }
}

pub fn unzip(path: &Path, dst: &Path) -> io::Result<()> {
    if dst.exists() {
        fs::remove_dir_all(dst)?;
    }

    fs::create_dir_all(dst)
        .and_then(|_| Command::new("unzip")
            .arg(path)
            .arg("-d")
            .arg(dst)
            .status()
            .and_then(|x| if x.success() {
                Ok(())
            } else {
                Err(io::Error::new(io::ErrorKind::Other, "tar command failed"))
            })
        )
}

pub fn untar(path: &Path, dst: &Path) -> io::Result<()> {
    if dst.exists() {
        fs::remove_dir_all(dst)?;
    }

    fs::create_dir_all(dst)
        .and_then(|_| Command::new("tar")
            .arg("-xvf")
            .arg(path)
            .arg("-C")
            .arg(dst)
            .args(&["--strip-components", "1"])
            .status()
            .and_then(|x| if x.success() {
                Ok(())
            } else {
                Err(io::Error::new(io::ErrorKind::Other, "tar command failed"))
            })
        )
}

pub fn mv_to_pool<P: AsRef<Path>>(path: P, archive: &str) -> io::Result<()> {
    pool(path.as_ref(), archive, |src, dst| fs::rename(src, dst))
}

pub fn cp_to_pool<P: AsRef<Path>>(path: P, archive: &str) -> io::Result<()> {
    pool(path.as_ref(), archive, |src, dst| fs::copy(src, dst).map(|_| ()))
}

fn pool<F: Fn(&Path, &Path) -> io::Result<()>>(path: &Path, archive: &str, action: F) -> io::Result<()> {
    for entry in path.read_dir()? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            continue;
        }

        let filename = path.file_name().and_then(|x| x.to_str());
        let filestem = path.file_stem().and_then(|x| x.to_str());

        if let (Some(filename), Some(filestem)) = (filename, filestem) {
            let mut package = &filename[..filename.find('_').unwrap_or(0)];

            let is_source = ["dsc", "tar.xz"].into_iter().any(|ext| filename.ends_with(ext));
            let destination = if is_source {
                PathBuf::from(
                    ["repo/pool/", archive, "/main/source/", &package[0..1], "/", package].concat()
                )
            } else {
                if package.ends_with("-dbgsym") {
                    package = &package[..package.len() - 7];
                }

                let arch = &filestem[filestem.rfind('_').unwrap_or(0) + 1..];
                PathBuf::from(
                    ["repo/pool/", archive, "/main/binary-", arch, "/", &package[0..1], "/", package].concat(),
                )
            };

            eprintln!("creating in pool: {:?}", destination);
            fs::create_dir_all(&destination)?;
            action(&path, &destination.join(filename))?;
        }
    }

    Ok(())
}

// NOTE: The following functions are implemented within Rust's standard in 1.26.0

fn initial_buffer_size(file: &File) -> usize {
    file.metadata().ok().map_or(0, |x| x.len()) as usize
}

pub fn read_to_string<P: AsRef<Path>>(path: P) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut string = String::with_capacity(initial_buffer_size(&file));
    file.read_to_string(&mut string)?;
    Ok(string)
}

pub fn read<P: AsRef<Path>>(path: P) -> io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let mut bytes = Vec::with_capacity(initial_buffer_size(&file));
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

pub fn write<P: AsRef<Path>, C: AsRef<[u8]>>(path: P, contents: C) -> io::Result<()> {
    File::create(path)?.write_all(contents.as_ref())
}
