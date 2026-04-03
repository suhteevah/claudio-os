//! Network utility commands for ClaudioOS shell.
//!
//! Provides ping, wget, curl, netstat, ifconfig, dns, traceroute, and nslookup
//! as shell builtins. These operate directly on the smoltcp `NetworkStack`.

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use claudio_net::{Instant, NetworkStack};
use claudio_net::tls::{tcp_connect, tcp_send, tcp_recv, tcp_close};

// ---------------------------------------------------------------------------
// Port counter for ephemeral ports (nettools-specific range)
// ---------------------------------------------------------------------------

static NETTOOLS_PORT: core::sync::atomic::AtomicU16 = core::sync::atomic::AtomicU16::new(60000);

fn next_port() -> u16 {
    let p = NETTOOLS_PORT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    if p >= 65500 {
        NETTOOLS_PORT.store(60000, core::sync::atomic::Ordering::Relaxed);
    }
    p
}

// ---------------------------------------------------------------------------
// ping — TCP-based RTT measurement (ICMP requires smoltcp "socket-icmp" feature)
// ---------------------------------------------------------------------------

/// Ping a host by measuring TCP connect + close RTT.
///
/// Since ICMP sockets are not enabled in the smoltcp feature set, we use
/// TCP SYN to port 80 (or 443) as a proxy for round-trip time measurement.
/// This is similar to `tcping` / `hping --syn`.
pub fn ping(
    stack: &mut NetworkStack,
    host: &str,
    count: usize,
    now: fn() -> Instant,
) -> String {
    let mut output = String::new();

    // Resolve hostname.
    let ip = match claudio_net::dns::resolve(stack, host, || now()) {
        Ok(ip) => ip,
        Err(e) => return format!("ping: cannot resolve '{}': {:?}\n", host, e),
    };

    output.push_str(&format!(
        "TCPING {} ({}) — TCP SYN to port 80, {} attempts\n",
        host, ip, count
    ));

    let mut rtts: Vec<i64> = Vec::new();
    let mut lost = 0usize;

    for seq in 0..count {
        let port = next_port();
        let t0 = now().total_millis();

        match tcp_connect(stack, ip, 80, port, || now()) {
            Ok(handle) => {
                let t1 = now().total_millis();
                let rtt = t1 - t0;
                tcp_close(stack, handle);
                output.push_str(&format!(
                    "  seq={} port=80 time={}ms\n",
                    seq, rtt
                ));
                rtts.push(rtt);
            }
            Err(e) => {
                output.push_str(&format!(
                    "  seq={} port=80 error: {:?}\n",
                    seq, e
                ));
                lost += 1;
            }
        }
    }

    // Statistics.
    let total = count;
    let received = total - lost;
    let loss_pct = if total > 0 { (lost * 100) / total } else { 0 };

    output.push_str(&format!(
        "\n--- {} tcping statistics ---\n",
        host
    ));
    output.push_str(&format!(
        "{} packets transmitted, {} received, {}% loss\n",
        total, received, loss_pct
    ));

    if !rtts.is_empty() {
        let min = rtts.iter().copied().min().unwrap_or(0);
        let max = rtts.iter().copied().max().unwrap_or(0);
        let sum: i64 = rtts.iter().sum();
        let avg = sum / rtts.len() as i64;
        output.push_str(&format!(
            "rtt min/avg/max = {}/{}/{} ms\n",
            min, avg, max
        ));
    }

    output
}

// ---------------------------------------------------------------------------
// dns / nslookup — DNS resolution
// ---------------------------------------------------------------------------

/// Resolve a hostname and display the IP address.
pub fn dns_lookup(
    stack: &mut NetworkStack,
    hostname: &str,
    now: fn() -> Instant,
) -> String {
    match claudio_net::dns::resolve(stack, hostname, || now()) {
        Ok(ip) => format!("{} -> {}\n", hostname, ip),
        Err(e) => format!("dns: failed to resolve '{}': {:?}\n", hostname, e),
    }
}

/// nslookup — DNS lookup with detail (servers, query type).
pub fn nslookup(
    stack: &mut NetworkStack,
    hostname: &str,
    now: fn() -> Instant,
) -> String {
    let mut output = String::new();

    // Show DNS server info.
    if stack.dns_servers.is_empty() {
        output.push_str("Server:  (none — DHCP not complete)\n");
    } else {
        for (i, srv) in stack.dns_servers.iter().enumerate() {
            if i == 0 {
                output.push_str(&format!("Server:  {}\n", srv));
            } else {
                output.push_str(&format!("         {}\n", srv));
            }
        }
    }
    output.push('\n');

    output.push_str(&format!("Name:    {}\n", hostname));

    match claudio_net::dns::resolve(stack, hostname, || now()) {
        Ok(ip) => {
            output.push_str(&format!("Address: {}\n", ip));
        }
        Err(e) => {
            output.push_str(&format!("** Query failed: {:?} **\n", e));
        }
    }

    output
}

// ---------------------------------------------------------------------------
// ifconfig — show network interface status
// ---------------------------------------------------------------------------

/// Display network interface configuration.
pub fn ifconfig(stack: &NetworkStack) -> String {
    let mut output = String::new();

    output.push_str("eth0      ");

    // MAC address.
    let hw = stack.iface.hardware_addr();
    output.push_str(&format!("HWaddr {}\n", hw));

    // IP addresses.
    let mut has_ip = false;
    for cidr in stack.iface.ip_addrs() {
        output.push_str(&format!("          inet {}\n", cidr));
        has_ip = true;
    }
    if !has_ip {
        output.push_str("          inet (no address — DHCP pending)\n");
    }

    // Gateway.
    match stack.gateway {
        Some(gw) => output.push_str(&format!("          gateway {}\n", gw)),
        None => output.push_str("          gateway (none)\n"),
    }

    // DNS servers.
    if !stack.dns_servers.is_empty() {
        for dns in &stack.dns_servers {
            output.push_str(&format!("          dns {}\n", dns));
        }
    }

    // Link status.
    if stack.has_ip {
        output.push_str("          UP RUNNING\n");
    } else {
        output.push_str("          DOWN (no IP)\n");
    }

    // Note: VirtIO device stats not yet tracked — would need counters in SmoltcpDevice.
    output.push_str("          (packet counters not yet implemented)\n");

    output
}

// ---------------------------------------------------------------------------
// netstat — list open TCP sockets
// ---------------------------------------------------------------------------

/// List network connection information.
///
/// smoltcp 0.12's SocketSet does not provide a public iterator over all sockets.
/// Instead we report the interface state and known socket counts.
pub fn netstat(stack: &NetworkStack) -> String {
    let mut output = String::new();

    output.push_str("Active Internet connections\n");
    output.push_str("Proto  Local Address          State\n");
    output.push_str("-----  ---------------------  -----------\n");

    // Report the interface IP addresses.
    for cidr in stack.iface.ip_addrs() {
        output.push_str(&format!("ip     {:<22} CONFIGURED\n", cidr));
    }

    // DHCP socket is always present.
    output.push_str(&format!(
        "dhcp   0.0.0.0:68             {}\n",
        if stack.has_ip { "BOUND" } else { "DISCOVERING" }
    ));

    // Gateway info.
    if let Some(gw) = stack.gateway {
        output.push_str(&format!("\nDefault gateway: {}\n", gw));
    }

    // DNS servers.
    if !stack.dns_servers.is_empty() {
        output.push_str("DNS servers:");
        for dns in &stack.dns_servers {
            output.push_str(&format!(" {}", dns));
        }
        output.push('\n');
    }

    output.push_str("\nNote: Per-socket enumeration requires smoltcp SocketSet iteration support.\n");
    output.push_str("TCP connections are created and destroyed transiently by nettools commands.\n");

    output
}

// ---------------------------------------------------------------------------
// wget — HTTP/HTTPS GET, print or save output
// ---------------------------------------------------------------------------

/// Fetch a URL via HTTP or HTTPS and return the response body as a string.
///
/// If `output_path` is Some, the body would be saved to the VFS (currently
/// stubbed — just notes the path). Otherwise the body is returned for display.
pub fn wget(
    stack: &mut NetworkStack,
    url: &str,
    output_path: Option<&str>,
    now: fn() -> Instant,
) -> String {
    let (scheme, host, port, path) = match parse_url(url) {
        Ok(parts) => parts,
        Err(e) => return format!("wget: {}\n", e),
    };

    let mut output = String::new();
    output.push_str(&format!("--  {}  --\n", url));
    output.push_str(&format!("Resolving {}...", host));

    let ip = match claudio_net::dns::resolve(stack, &host, || now()) {
        Ok(ip) => {
            output.push_str(&format!(" {}\n", ip));
            ip
        }
        Err(e) => return format!("{}wget: DNS failed: {:?}\n", output, e),
    };

    output.push_str(&format!("Connecting to {}:{}...", ip, port));

    // Build HTTP request.
    let req = claudio_net::HttpRequest::get(&host, &path)
        .header("User-Agent", "ClaudioOS/0.1 wget")
        .header("Accept", "*/*")
        .header("Connection", "close");
    let req_bytes = req.to_bytes();

    let response_bytes = if scheme == "https" {
        let rng_seed = now().total_millis() as u64;
        match claudio_net::https_request(stack, &host, port, &req_bytes, now, rng_seed) {
            Ok(data) => {
                output.push_str(" connected.\n");
                data
            }
            Err(e) => return format!("{}wget: HTTPS error: {:?}\n", output, e),
        }
    } else {
        // Plain HTTP via raw TCP.
        let local_port = next_port();
        let handle = match tcp_connect(stack, ip, port, local_port, || now()) {
            Ok(h) => {
                output.push_str(" connected.\n");
                h
            }
            Err(e) => return format!("{}wget: TCP connect failed: {:?}\n", output, e),
        };

        if let Err(e) = tcp_send(stack, handle, &req_bytes, || now()) {
            tcp_close(stack, handle);
            return format!("{}wget: send failed: {:?}\n", output, e);
        }

        // Read response.
        let mut resp_buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match tcp_recv(stack, handle, &mut chunk, || now()) {
                Ok(0) => break,
                Ok(n) => resp_buf.extend_from_slice(&chunk[..n]),
                Err(_) => break,
            }
        }
        tcp_close(stack, handle);
        resp_buf
    };

    // Parse HTTP response.
    match claudio_net::HttpResponse::parse(&response_bytes) {
        Ok(resp) => {
            output.push_str(&format!("HTTP {} {}\n", resp.status, resp.reason));
            output.push_str(&format!("Length: {} bytes\n", resp.body.len()));

            if let Some(path) = output_path {
                // VFS save would go here. For now, note it.
                output.push_str(&format!("Saving to: '{}' (VFS not mounted — printing instead)\n\n", path));
                // Fall through to print body.
            }

            // Print body as text (lossy UTF-8).
            let body_text = String::from_utf8_lossy(&resp.body);
            // Truncate very large responses for display.
            if body_text.len() > 8192 {
                output.push_str(&body_text[..8192]);
                output.push_str("\n\n... (truncated, ");
                output.push_str(&format!("{} bytes total)\n", resp.body.len()));
            } else {
                output.push_str(&body_text);
                if !body_text.ends_with('\n') {
                    output.push('\n');
                }
            }
        }
        Err(e) => {
            output.push_str(&format!("wget: failed to parse HTTP response: {:?}\n", e));
            // Show raw bytes (first 512).
            let preview_len = core::cmp::min(response_bytes.len(), 512);
            let preview = String::from_utf8_lossy(&response_bytes[..preview_len]);
            output.push_str(&format!("Raw ({} bytes): {}\n", response_bytes.len(), preview));
        }
    }

    output
}

// ---------------------------------------------------------------------------
// curl — HTTP GET/POST with headers
// ---------------------------------------------------------------------------

/// curl-like HTTP client.
///
/// Supports:
///   curl <url>                        — GET request, print response
///   curl -X POST <url>                — POST request
///   curl -H "Name: Value" <url>       — custom header
///   curl -d "body" <url>              — POST with body data
///   curl -v <url>                     — verbose (show request + response headers)
pub fn curl(
    stack: &mut NetworkStack,
    args: &str,
    now: fn() -> Instant,
) -> String {
    // Parse curl arguments.
    let mut method = "GET";
    let mut url = "";
    let mut extra_headers: Vec<(&str, &str)> = Vec::new();
    let mut body_data: Option<&str> = None;
    let mut verbose = false;

    let tokens = shell_tokenize(args);
    let mut i = 0;
    while i < tokens.len() {
        match tokens[i].as_str() {
            "-X" => {
                if i + 1 < tokens.len() {
                    method = leak_str(&tokens[i + 1]);
                    i += 2;
                } else {
                    return "curl: -X requires a method argument\n".into();
                }
            }
            "-H" => {
                if i + 1 < tokens.len() {
                    let hdr = &tokens[i + 1];
                    if let Some(colon_pos) = hdr.find(':') {
                        let name = hdr[..colon_pos].trim();
                        let value = hdr[colon_pos + 1..].trim();
                        extra_headers.push((leak_str_ref(name), leak_str_ref(value)));
                    }
                    i += 2;
                } else {
                    return "curl: -H requires a header argument\n".into();
                }
            }
            "-d" => {
                if i + 1 < tokens.len() {
                    body_data = Some(leak_str(&tokens[i + 1]));
                    if method == "GET" {
                        method = "POST"; // -d implies POST.
                    }
                    i += 2;
                } else {
                    return "curl: -d requires a data argument\n".into();
                }
            }
            "-v" => {
                verbose = true;
                i += 1;
            }
            s if !s.starts_with('-') => {
                url = leak_str(&tokens[i]);
                i += 1;
            }
            _ => {
                i += 1; // Skip unknown flags.
            }
        }
    }

    if url.is_empty() {
        return "curl: no URL specified\nUsage: curl [-X METHOD] [-H \"Header: Value\"] [-d body] [-v] <url>\n".into();
    }

    let (scheme, host, port, path) = match parse_url(url) {
        Ok(parts) => parts,
        Err(e) => return format!("curl: {}\n", e),
    };

    // Resolve DNS.
    let ip = match claudio_net::dns::resolve(stack, &host, || now()) {
        Ok(ip) => ip,
        Err(e) => return format!("curl: DNS failed for '{}': {:?}\n", host, e),
    };

    // Build HTTP request.
    let body_bytes = body_data.map(|s| s.as_bytes().to_vec());
    let mut req = if let Some(ref body) = body_bytes {
        claudio_net::HttpRequest::post(&host, &path, body.clone())
    } else {
        claudio_net::HttpRequest::get(&host, &path)
    };

    // Override method if -X was used.
    // HttpRequest has method as &'static str, so we need to match known methods.
    req.method = match method {
        "GET" => "GET",
        "POST" => "POST",
        "PUT" => "PUT",
        "DELETE" => "DELETE",
        "PATCH" => "PATCH",
        "HEAD" => "HEAD",
        "OPTIONS" => "OPTIONS",
        _ => "GET",
    };

    req.headers.push(("User-Agent".into(), "ClaudioOS/0.1 curl".into()));
    req.headers.push(("Accept".into(), "*/*".into()));
    req.headers.push(("Connection".into(), "close".into()));

    for (name, value) in &extra_headers {
        req.headers.push(((*name).into(), (*value).into()));
    }

    let req_bytes = req.to_bytes();

    let mut output = String::new();

    if verbose {
        output.push_str(&format!("> {} {} HTTP/1.1\n", req.method, path));
        output.push_str(&format!("> Host: {}\n", host));
        for (n, v) in &req.headers {
            output.push_str(&format!("> {}: {}\n", n, v));
        }
        output.push_str(">\n");
    }

    let response_bytes = if scheme == "https" {
        let rng_seed = now().total_millis() as u64;
        match claudio_net::https_request(stack, &host, port, &req_bytes, now, rng_seed) {
            Ok(data) => data,
            Err(e) => return format!("{}curl: HTTPS error: {:?}\n", output, e),
        }
    } else {
        // Plain HTTP.
        let local_port = next_port();
        let handle = match tcp_connect(stack, ip, port, local_port, || now()) {
            Ok(h) => h,
            Err(e) => return format!("{}curl: TCP connect failed: {:?}\n", output, e),
        };

        if let Err(e) = tcp_send(stack, handle, &req_bytes, || now()) {
            tcp_close(stack, handle);
            return format!("{}curl: send failed: {:?}\n", output, e);
        }

        let mut resp_buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match tcp_recv(stack, handle, &mut chunk, || now()) {
                Ok(0) => break,
                Ok(n) => resp_buf.extend_from_slice(&chunk[..n]),
                Err(_) => break,
            }
        }
        tcp_close(stack, handle);
        resp_buf
    };

    // Parse and display response.
    match claudio_net::HttpResponse::parse(&response_bytes) {
        Ok(resp) => {
            if verbose {
                output.push_str(&format!("< HTTP/1.1 {} {}\n", resp.status, resp.reason));
                for (n, v) in &resp.headers {
                    output.push_str(&format!("< {}: {}\n", n, v));
                }
                output.push_str("<\n");
            }

            let body_text = String::from_utf8_lossy(&resp.body);
            output.push_str(&body_text);
            if !body_text.is_empty() && !body_text.ends_with('\n') {
                output.push('\n');
            }
        }
        Err(_) => {
            // Couldn't parse — dump raw.
            let raw_text = String::from_utf8_lossy(&response_bytes);
            output.push_str(&raw_text);
            if !raw_text.is_empty() && !raw_text.ends_with('\n') {
                output.push('\n');
            }
        }
    }

    output
}

// ---------------------------------------------------------------------------
// traceroute — simplified TTL-incrementing probe (TCP-based)
// ---------------------------------------------------------------------------

/// Simplified traceroute using incremental TCP connection attempts.
///
/// True ICMP-based traceroute requires the `socket-icmp` smoltcp feature.
/// This version attempts TCP connections and measures RTT at each step.
/// On a NAT/SLIRP network (QEMU), this will only show the final hop.
pub fn traceroute(
    stack: &mut NetworkStack,
    host: &str,
    now: fn() -> Instant,
) -> String {
    let mut output = String::new();

    let ip = match claudio_net::dns::resolve(stack, host, || now()) {
        Ok(ip) => ip,
        Err(e) => return format!("traceroute: cannot resolve '{}': {:?}\n", host, e),
    };

    output.push_str(&format!(
        "traceroute to {} ({}), TCP-based (ICMP not available)\n",
        host, ip
    ));
    output.push_str("Note: True ICMP traceroute requires smoltcp 'socket-icmp' feature.\n");
    output.push_str("      Showing TCP SYN probe to port 80.\n\n");

    // Do a single TCP connect to measure the RTT to the destination.
    let port = next_port();
    let t0 = now().total_millis();

    match tcp_connect(stack, ip, 80, port, || now()) {
        Ok(handle) => {
            let t1 = now().total_millis();
            tcp_close(stack, handle);
            output.push_str(&format!(
                " 1  {} ({})  {} ms\n",
                host, ip, t1 - t0
            ));
        }
        Err(e) => {
            output.push_str(&format!(
                " 1  {} ({})  * (TCP error: {:?})\n",
                host, ip, e
            ));
        }
    }

    output
}

// ---------------------------------------------------------------------------
// Dispatch — match shell input to a network command
// ---------------------------------------------------------------------------

/// Try to handle a shell command as a network utility.
///
/// Returns `Some(output_string)` if the command was recognized as a network
/// tool, `None` if it should be passed to the regular shell.
pub fn try_handle_netcmd(
    input: &str,
    stack: &mut NetworkStack,
    now: fn() -> Instant,
) -> Option<String> {
    let trimmed = input.trim();
    let (cmd, rest) = match trimmed.split_once(|c: char| c.is_whitespace()) {
        Some((c, r)) => (c, r.trim()),
        None => (trimmed, ""),
    };

    match cmd {
        "ping" => {
            if rest.is_empty() {
                return Some("Usage: ping <host> [count]\n".into());
            }
            let parts: Vec<&str> = rest.split_whitespace().collect();
            let host = parts[0];
            let count = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(4);
            Some(ping(stack, host, count, now))
        }

        "wget" => {
            if rest.is_empty() {
                return Some("Usage: wget <url> [output_path]\n".into());
            }
            let parts: Vec<&str> = rest.splitn(2, char::is_whitespace).collect();
            let url = parts[0];
            let out = parts.get(1).map(|s| s.trim());
            Some(wget(stack, url, out, now))
        }

        "curl" => {
            if rest.is_empty() {
                return Some("Usage: curl [-X METHOD] [-H \"Header: Value\"] [-d body] [-v] <url>\n".into());
            }
            Some(curl(stack, rest, now))
        }

        "netstat" => {
            Some(netstat(stack))
        }

        "ifconfig" => {
            Some(ifconfig(stack))
        }

        "dns" => {
            if rest.is_empty() {
                return Some("Usage: dns <hostname>\n".into());
            }
            Some(dns_lookup(stack, rest.split_whitespace().next().unwrap_or(""), now))
        }

        "nslookup" => {
            if rest.is_empty() {
                return Some("Usage: nslookup <hostname>\n".into());
            }
            Some(nslookup(stack, rest.split_whitespace().next().unwrap_or(""), now))
        }

        "traceroute" => {
            if rest.is_empty() {
                return Some("Usage: traceroute <host>\n".into());
            }
            Some(traceroute(stack, rest.split_whitespace().next().unwrap_or(""), now))
        }

        _ => None,
    }
}

// ---------------------------------------------------------------------------
// URL parser
// ---------------------------------------------------------------------------

/// Parse a URL into (scheme, host, port, path).
fn parse_url(url: &str) -> Result<(String, String, u16, String), String> {
    let (scheme, after_scheme) = if url.starts_with("https://") {
        ("https".into(), &url[8..])
    } else if url.starts_with("http://") {
        ("http".into(), &url[7..])
    } else {
        // Default to http.
        ("http".into(), url)
    };

    let (host_port, path) = match after_scheme.find('/') {
        Some(i) => (&after_scheme[..i], &after_scheme[i..]),
        None => (after_scheme, "/"),
    };

    let (host, port) = match host_port.find(':') {
        Some(i) => {
            let h = &host_port[..i];
            let p = host_port[i + 1..]
                .parse::<u16>()
                .map_err(|_| format!("invalid port in '{}'", host_port))?;
            (h.into(), p)
        }
        None => {
            let default_port: u16 = if scheme == "https" { 443 } else { 80 };
            (host_port.into(), default_port)
        }
    };

    Ok((scheme, host, port, path.into()))
}

// ---------------------------------------------------------------------------
// Shell tokenizer (handles quoted strings)
// ---------------------------------------------------------------------------

fn shell_tokenize(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    for ch in input.chars() {
        match ch {
            '\'' if !in_double_quote => {
                in_single_quote = !in_single_quote;
            }
            '"' if !in_single_quote => {
                in_double_quote = !in_double_quote;
            }
            ' ' | '\t' if !in_single_quote && !in_double_quote => {
                if !current.is_empty() {
                    tokens.push(core::mem::take(&mut current));
                }
            }
            _ => {
                current.push(ch);
            }
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

// ---------------------------------------------------------------------------
// Helpers for lifetime wrangling (bare-metal, single-threaded, no drops needed)
// ---------------------------------------------------------------------------

/// Leak a String to get a &'static str.
/// In a bare-metal single-run system this is fine — memory is reclaimed at reboot.
fn leak_str(s: &str) -> &'static str {
    let boxed = alloc::boxed::Box::leak(String::from(s).into_boxed_str());
    boxed
}

fn leak_str_ref(s: &str) -> &'static str {
    leak_str(s)
}
