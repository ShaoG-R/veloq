use std::path::Path;
use crate::io::op::Open;
use crate::io::op::Op;

#[derive(Clone, Debug)]
pub struct OpenOptions {
    read: bool,
    write: bool,
    append: bool,
    truncate: bool,
    create: bool,
    create_new: bool,
    mode: u32,
}

impl OpenOptions {
    pub fn new() -> Self {
        Self {
            read: false,
            write: false,
            append: false,
            truncate: false,
            create: false,
            create_new: false,
            mode: 0o666,
        }
    }

    pub fn read(&mut self, read: bool) -> &mut Self {
        self.read = read;
        self
    }

    pub fn write(&mut self, write: bool) -> &mut Self {
        self.write = write;
        self
    }

    pub fn append(&mut self, append: bool) -> &mut Self {
        self.append = append;
        self
    }

    pub fn truncate(&mut self, truncate: bool) -> &mut Self {
        self.truncate = truncate;
        self
    }

    pub fn create(&mut self, create: bool) -> &mut Self {
        self.create = create;
        self
    }

    pub fn create_new(&mut self, create_new: bool) -> &mut Self {
        self.create_new = create_new;
        self
    }

    pub fn mode(&mut self, mode: u32) -> &mut Self {
        self.mode = mode;
        self
    }

    pub async fn open(&self, path: impl AsRef<Path>) -> std::io::Result<super::file::File> {
        let path = path.as_ref();
        
        #[cfg(unix)]
        let (path_c, flags) = {
            use std::os::unix::ffi::OsStrExt;
            let path_c = std::ffi::CString::new(path.as_os_str().as_bytes())?;
            let mut flags = 0;
            
            if self.read && !self.write && !self.append {
                flags |= libc::O_RDONLY;
            } else if !self.read && self.write && !self.append {
                flags |= libc::O_WRONLY;
            } else if self.append {
                flags |= libc::O_WRONLY | libc::O_APPEND;
            } else {
                flags |= libc::O_RDWR;
            }
            
            if self.create { flags |= libc::O_CREAT; }
            if self.create_new { flags |= libc::O_EXCL | libc::O_CREAT; }
            if self.truncate { flags |= libc::O_TRUNC; }
            
            (path_c, flags)
        };

        #[cfg(windows)]
        let (path_w, flags, mode) = {
             use std::os::windows::ffi::OsStrExt;
             let mut path_w: Vec<u16> = path.as_os_str().encode_wide().collect();
             path_w.push(0);
             
             // Mapping to Win32 constants
             const GENERIC_READ: u32 = 0x80000000;
             const GENERIC_WRITE: u32 = 0x40000000;
             const OPEN_EXISTING: u32 = 3;
             const CREATE_NEW: u32 = 1;
             const CREATE_ALWAYS: u32 = 2; // Overwrite
             const OPEN_ALWAYS: u32 = 4; // Create if not exists
             const TRUNCATE_EXISTING: u32 = 5;
             
             let mut access = 0;
             if self.read { access |= GENERIC_READ; }
             if self.write { access |= GENERIC_WRITE; }
             if self.append { 
                 access |= 0x0004; // FILE_APPEND_DATA (approx)
             }
             
             let mut disposition = OPEN_EXISTING;
             if self.create_new {
                 disposition = CREATE_NEW;
             } else if self.create {
                 if self.truncate {
                     disposition = CREATE_ALWAYS;
                 } else {
                     disposition = OPEN_ALWAYS;
                 }
             } else if self.truncate {
                 disposition = TRUNCATE_EXISTING;
             }
             
             (path_w, access as i32, disposition)
        };

        #[cfg(unix)]
        let op = Open {
            path: path_c,
            flags,
            mode: self.mode,
        };

        #[cfg(windows)]
        let op = Open {
            path: path_w,
            flags: flags, // access
            mode: mode, // disposition
        };

        let driver = crate::runtime::current_driver();
        let (res, _) = Op::new(op, driver).await;
        let fd = res? as crate::io::op::SysRawOp;

        use crate::io::op::IoFd;
        Ok(super::file::File {
            fd: IoFd::Raw(fd),
        })
    }
}
