//! Minimal built-in libc for cc-lite.
//!
//! Provides C standard library functions that map to kernel services.
//! These are registered as built-in functions available to all C programs.

use alloc::vec::Vec;

/// A built-in libc function callable from compiled C code.
#[derive(Debug, Clone)]
pub struct LibcFunc {
    pub name: &'static str,
    pub impl_fn: LibcImpl,
}

/// Implementation type for libc functions.
#[derive(Debug, Clone, Copy)]
pub enum LibcImpl {
    Printf,
    Sprintf,
    Puts,
    Putchar,
    Getchar,
    Malloc,
    Free,
    Realloc,
    Calloc,
    Memcpy,
    Memset,
    Memmove,
    Memcmp,
    Strlen,
    Strcpy,
    Strncpy,
    Strcmp,
    Strncmp,
    Strcat,
    Strncat,
    Strchr,
    Strrchr,
    Strstr,
    Atoi,
    Atol,
    Atof,
    Strtol,
    Strtod,
    Fopen,
    Fclose,
    Fread,
    Fwrite,
    Fprintf,
    Fgets,
    Fputs,
    Exit,
    Abort,
}

/// Get all built-in libc functions.
pub fn builtins() -> Vec<LibcFunc> {
    alloc::vec![
        LibcFunc { name: "printf", impl_fn: LibcImpl::Printf },
        LibcFunc { name: "sprintf", impl_fn: LibcImpl::Sprintf },
        LibcFunc { name: "puts", impl_fn: LibcImpl::Puts },
        LibcFunc { name: "putchar", impl_fn: LibcImpl::Putchar },
        LibcFunc { name: "getchar", impl_fn: LibcImpl::Getchar },
        LibcFunc { name: "malloc", impl_fn: LibcImpl::Malloc },
        LibcFunc { name: "free", impl_fn: LibcImpl::Free },
        LibcFunc { name: "realloc", impl_fn: LibcImpl::Realloc },
        LibcFunc { name: "calloc", impl_fn: LibcImpl::Calloc },
        LibcFunc { name: "memcpy", impl_fn: LibcImpl::Memcpy },
        LibcFunc { name: "memset", impl_fn: LibcImpl::Memset },
        LibcFunc { name: "memmove", impl_fn: LibcImpl::Memmove },
        LibcFunc { name: "memcmp", impl_fn: LibcImpl::Memcmp },
        LibcFunc { name: "strlen", impl_fn: LibcImpl::Strlen },
        LibcFunc { name: "strcpy", impl_fn: LibcImpl::Strcpy },
        LibcFunc { name: "strncpy", impl_fn: LibcImpl::Strncpy },
        LibcFunc { name: "strcmp", impl_fn: LibcImpl::Strcmp },
        LibcFunc { name: "strncmp", impl_fn: LibcImpl::Strncmp },
        LibcFunc { name: "strcat", impl_fn: LibcImpl::Strcat },
        LibcFunc { name: "strncat", impl_fn: LibcImpl::Strncat },
        LibcFunc { name: "strchr", impl_fn: LibcImpl::Strchr },
        LibcFunc { name: "strrchr", impl_fn: LibcImpl::Strrchr },
        LibcFunc { name: "strstr", impl_fn: LibcImpl::Strstr },
        LibcFunc { name: "atoi", impl_fn: LibcImpl::Atoi },
        LibcFunc { name: "atol", impl_fn: LibcImpl::Atol },
        LibcFunc { name: "atof", impl_fn: LibcImpl::Atof },
        LibcFunc { name: "strtol", impl_fn: LibcImpl::Strtol },
        LibcFunc { name: "strtod", impl_fn: LibcImpl::Strtod },
        LibcFunc { name: "fopen", impl_fn: LibcImpl::Fopen },
        LibcFunc { name: "fclose", impl_fn: LibcImpl::Fclose },
        LibcFunc { name: "fread", impl_fn: LibcImpl::Fread },
        LibcFunc { name: "fwrite", impl_fn: LibcImpl::Fwrite },
        LibcFunc { name: "fprintf", impl_fn: LibcImpl::Fprintf },
        LibcFunc { name: "fgets", impl_fn: LibcImpl::Fgets },
        LibcFunc { name: "fputs", impl_fn: LibcImpl::Fputs },
        LibcFunc { name: "exit", impl_fn: LibcImpl::Exit },
        LibcFunc { name: "abort", impl_fn: LibcImpl::Abort },
    ]
}

/// Format a printf-style format string with arguments.
///
/// Supports: %d, %i, %u, %x, %X, %o, %s, %c, %p, %f, %e, %g, %ld, %lld, %lu, %llu, %zu, %%
///
/// Arguments are passed as raw i64 values (as they would be in registers).
pub fn format_printf(fmt: &[u8], args: &[i64]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut arg_idx = 0;

    while i < fmt.len() {
        if fmt[i] == b'%' {
            i += 1;
            if i >= fmt.len() {
                break;
            }

            // Flags
            let mut zero_pad = false;
            let mut left_align = false;
            let mut width: usize = 0;

            // Parse flags
            loop {
                match fmt.get(i) {
                    Some(b'0') => { zero_pad = true; i += 1; }
                    Some(b'-') => { left_align = true; i += 1; }
                    _ => break,
                }
            }

            // Parse width
            while i < fmt.len() && fmt[i].is_ascii_digit() {
                width = width * 10 + (fmt[i] - b'0') as usize;
                i += 1;
            }

            // Parse length modifier
            let mut long_count = 0u8;
            let mut is_size_t = false;
            while i < fmt.len() {
                match fmt[i] {
                    b'l' => { long_count += 1; i += 1; }
                    b'z' => { is_size_t = true; i += 1; }
                    b'h' => { i += 1; } // ignore 'h'
                    _ => break,
                }
            }

            if i >= fmt.len() {
                break;
            }

            let spec = fmt[i];
            i += 1;

            match spec {
                b'd' | b'i' => {
                    let val = args.get(arg_idx).copied().unwrap_or(0);
                    arg_idx += 1;
                    let s = format_int(val, 10, false);
                    pad_and_push(&mut out, &s, width, zero_pad, left_align);
                }
                b'u' => {
                    let val = args.get(arg_idx).copied().unwrap_or(0) as u64;
                    arg_idx += 1;
                    let s = format_uint(val, 10, false);
                    pad_and_push(&mut out, &s, width, zero_pad, left_align);
                }
                b'x' => {
                    let val = args.get(arg_idx).copied().unwrap_or(0) as u64;
                    arg_idx += 1;
                    let s = format_uint(val, 16, false);
                    pad_and_push(&mut out, &s, width, zero_pad, left_align);
                }
                b'X' => {
                    let val = args.get(arg_idx).copied().unwrap_or(0) as u64;
                    arg_idx += 1;
                    let s = format_uint(val, 16, true);
                    pad_and_push(&mut out, &s, width, zero_pad, left_align);
                }
                b'o' => {
                    let val = args.get(arg_idx).copied().unwrap_or(0) as u64;
                    arg_idx += 1;
                    let s = format_uint(val, 8, false);
                    pad_and_push(&mut out, &s, width, zero_pad, left_align);
                }
                b's' => {
                    let ptr = args.get(arg_idx).copied().unwrap_or(0);
                    arg_idx += 1;
                    // In real bare metal, this would read from memory at ptr
                    // For safety, just print "(string)"
                    out.extend_from_slice(b"(string)");
                }
                b'c' => {
                    let val = args.get(arg_idx).copied().unwrap_or(0) as u8;
                    arg_idx += 1;
                    out.push(val);
                }
                b'p' => {
                    let val = args.get(arg_idx).copied().unwrap_or(0) as u64;
                    arg_idx += 1;
                    out.extend_from_slice(b"0x");
                    let s = format_uint(val, 16, false);
                    out.extend_from_slice(&s);
                }
                b'f' | b'e' | b'g' => {
                    let val = args.get(arg_idx).copied().unwrap_or(0);
                    arg_idx += 1;
                    let f = f64::from_bits(val as u64);
                    let s = format_float(f);
                    out.extend_from_slice(&s);
                }
                b'%' => {
                    out.push(b'%');
                }
                _ => {
                    out.push(b'%');
                    out.push(spec);
                }
            }
        } else {
            out.push(fmt[i]);
            i += 1;
        }
    }

    out
}

fn format_int(val: i64, base: u64, _upper: bool) -> Vec<u8> {
    if val == 0 {
        return alloc::vec![b'0'];
    }
    let mut buf = Vec::new();
    let neg = val < 0;
    let mut v = if neg { (val as i128).wrapping_neg() as u64 } else { val as u64 };
    while v > 0 {
        let d = (v % base) as u8;
        buf.push(if d < 10 { b'0' + d } else { b'a' + d - 10 });
        v /= base;
    }
    if neg {
        buf.push(b'-');
    }
    buf.reverse();
    buf
}

fn format_uint(val: u64, base: u64, upper: bool) -> Vec<u8> {
    if val == 0 {
        return alloc::vec![b'0'];
    }
    let mut buf = Vec::new();
    let mut v = val;
    while v > 0 {
        let d = (v % base) as u8;
        let ch = if d < 10 {
            b'0' + d
        } else if upper {
            b'A' + d - 10
        } else {
            b'a' + d - 10
        };
        buf.push(ch);
        v /= base;
    }
    buf.reverse();
    buf
}

fn format_float(val: f64) -> Vec<u8> {
    // Simple float formatting (6 decimal places)
    let mut buf = Vec::new();
    if val < 0.0 {
        buf.push(b'-');
    }
    let v = if val < 0.0 { -val } else { val };
    let int_part = v as u64;
    let frac_part = ((v - int_part as f64) * 1_000_000.0) as u64;

    let int_str = format_uint(int_part, 10, false);
    buf.extend_from_slice(&int_str);
    buf.push(b'.');
    // Pad fractional part with leading zeros
    let frac_str = format_uint(frac_part, 10, false);
    for _ in frac_str.len()..6 {
        buf.push(b'0');
    }
    buf.extend_from_slice(&frac_str);
    buf
}

fn pad_and_push(out: &mut Vec<u8>, s: &[u8], width: usize, zero_pad: bool, left_align: bool) {
    if width > s.len() && !left_align {
        let pad = if zero_pad { b'0' } else { b' ' };
        for _ in 0..(width - s.len()) {
            out.push(pad);
        }
    }
    out.extend_from_slice(s);
    if width > s.len() && left_align {
        for _ in 0..(width - s.len()) {
            out.push(b' ');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_printf_int() {
        let result = format_printf(b"x=%d", &[42]);
        assert_eq!(result, b"x=42");
    }

    #[test]
    fn test_printf_hex() {
        let result = format_printf(b"0x%x", &[255]);
        assert_eq!(result, b"0xff");
    }

    #[test]
    fn test_printf_char() {
        let result = format_printf(b"%c", &[65]); // 'A'
        assert_eq!(result, b"A");
    }

    #[test]
    fn test_printf_percent() {
        let result = format_printf(b"100%%", &[]);
        assert_eq!(result, b"100%");
    }

    #[test]
    fn test_printf_width() {
        let result = format_printf(b"%05d", &[42]);
        assert_eq!(result, b"00042");
    }

    #[test]
    fn test_format_negative() {
        let result = format_printf(b"%d", &[-1]);
        assert_eq!(result, b"-1");
    }
}
