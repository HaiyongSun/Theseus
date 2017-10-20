use ctypes::c_char;
use error::Error as StdError;
use ffi::{CStr, CString, OsStr, OsString};
use fmt;
use io;
use iter;
use libc;
use linux;
use marker::PhantomData;
use path::{self, PathBuf};
use slice;
use super::cvt;
use sys::ext::prelude::*;
use vec;

static ENV_LOCK: () = ();
// TODO(steed, #143): Synchronize environment access once we have mutexes.
trait MutexExt {
    fn lock(&self) { }
    fn unlock(&self) { }
}
impl MutexExt for () { }

pub fn errno() -> i32 {
    panic!("no C-compatible errno variable");
}

pub fn error_string(errno: i32) -> String {
    linux::errno::error_string(errno).map(|s| s.into()).unwrap_or_else(|| {
        format!("Unknown OS error ({})", errno)
    })
}

pub fn exit(code: i32) -> ! {
    unsafe { libc::exit_group(code) }
}

pub fn getcwd() -> io::Result<PathBuf> {
    let mut buf = Vec::with_capacity(512);
    loop {
        unsafe {
            let ptr = buf.as_mut_ptr() as *mut libc::c_char;
            match cvt(libc::getcwd(ptr, buf.capacity())) {
                Ok(_) => {
                    let len = CStr::from_ptr(buf.as_ptr() as *const libc::c_char).to_bytes().len();
                    buf.set_len(len);
                    buf.shrink_to_fit();
                    return Ok(PathBuf::from(OsString::from_vec(buf)));
                },
                Err(ref e) if e.raw_os_error() == Some(libc::ERANGE) => {},
                Err(e) => return Err(e),
            }

            // Trigger the internal buffer resizing logic of `Vec` by requiring
            // more space than the current capacity.
            let cap = buf.capacity();
            buf.set_len(cap);
            buf.reserve(1);
        }
    }
}

pub fn page_size() -> usize {
    // TODO(steed, #133): Implement me.
    unimplemented!();
}

pub fn chdir(p: &path::Path) -> io::Result<()> {
    let p: &OsStr = p.as_ref();
    let p = CString::new(p.as_bytes())?;
    unsafe {
        cvt(libc::chdir(p.as_ptr())).map(|_| ())
    }
}

pub struct SplitPaths<'a> {
    iter: iter::Map<slice::Split<'a, u8, fn(&u8) -> bool>,
                    fn(&'a [u8]) -> PathBuf>,
}

pub fn split_paths(unparsed: &OsStr) -> SplitPaths {
    fn bytes_to_path(b: &[u8]) -> PathBuf {
        PathBuf::from(<OsStr as OsStrExt>::from_bytes(b))
    }
    fn is_colon(b: &u8) -> bool { *b == b':' }
    let unparsed = unparsed.as_bytes();
    SplitPaths {
        iter: unparsed.split(is_colon as fn(&u8) -> bool)
                      .map(bytes_to_path as fn(&[u8]) -> PathBuf)
    }
}

impl<'a> Iterator for SplitPaths<'a> {
    type Item = PathBuf;
    fn next(&mut self) -> Option<PathBuf> { self.iter.next() }
    fn size_hint(&self) -> (usize, Option<usize>) { self.iter.size_hint() }
}

#[derive(Debug)]
pub struct JoinPathsError;

pub fn join_paths<I, T>(paths: I) -> Result<OsString, JoinPathsError>
    where I: Iterator<Item=T>, T: AsRef<OsStr>
{
    let mut joined = Vec::new();
    let sep = b':';

    for (i, path) in paths.enumerate() {
        let path = path.as_ref().as_bytes();
        if i > 0 { joined.push(sep) }
        if path.contains(&sep) {
            return Err(JoinPathsError)
        }
        joined.extend_from_slice(path);
    }
    Ok(OsStringExt::from_vec(joined))
}

impl fmt::Display for JoinPathsError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        "path segment contains separator `:`".fmt(f)
    }
}

impl StdError for JoinPathsError {
    fn description(&self) -> &str { "failed to join paths" }
}

pub fn current_exe() -> io::Result<PathBuf> {
    ::fs::read_link("/proc/self/exe")
}

pub unsafe fn environ() -> *const *const c_char {
    libc::environ()
}

pub struct Env {
    iter: vec::IntoIter<(OsString, OsString)>,
    _dont_send_or_sync_me: PhantomData<*mut ()>,
}

impl Iterator for Env {
    type Item = (OsString, OsString);
    fn next(&mut self) -> Option<(OsString, OsString)> { self.iter.next() }
    fn size_hint(&self) -> (usize, Option<usize>) { self.iter.size_hint() }
}

/// Returns a vector of (variable, value) byte-vector pairs for all the
/// environment variables of the current process.
pub fn env() -> Env {
    fn os_string(slice: &[u8]) -> OsString {
        OsString::from_vec(slice.to_owned())
    }
    unsafe {
        ENV_LOCK.lock();
        let result = Env {
            iter: libc::env().values()
                .map(|kv| (os_string(&kv.key), os_string(&kv.value)))
                .collect::<Vec<_>>()
                .into_iter(),
            _dont_send_or_sync_me: PhantomData,
        };
        ENV_LOCK.unlock();
        result
    }
}

pub fn getenv(k: &OsStr) -> io::Result<Option<OsString>> {
    // environment variables with a nul byte can't be set, so their value is
    // always None as well
    let k = CString::new(k.as_bytes())?;
    unsafe {
        ENV_LOCK.lock();
        let s = libc::getenv(k.as_bytes()).map(|v| OsString::from_vec(v.to_owned()));
        ENV_LOCK.unlock();
        return Ok(s)
    }
}

pub fn setenv(k: &OsStr, v: &OsStr) -> io::Result<()> {
    unsafe {
        ENV_LOCK.lock();
        let result = cvt(libc::setenv(k.as_bytes(), v.as_bytes())).map(|_| ());
        ENV_LOCK.unlock();
        result
    }
}

pub fn unsetenv(k: &OsStr) -> io::Result<()> {
    unsafe {
        ENV_LOCK.lock();
        let ret = cvt(libc::unsetenv(k.as_bytes())).map(|_| ());
        ENV_LOCK.unlock();
        return ret
    }
}

pub fn temp_dir() -> PathBuf {
    ::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(|| {
        if cfg!(target_os = "android") {
            PathBuf::from("/data/local/tmp")
        } else {
            PathBuf::from("/tmp")
        }
    })
}
