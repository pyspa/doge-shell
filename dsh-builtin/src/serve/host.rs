//! Restrict HTTP authorities before reading local files (DNS rebinding defense).
use axum::{
    extract::{Request, State},
    http::{StatusCode, Version, header, uri::Authority},
    middleware::Next,
    response::Response,
};
use std::net::IpAddr;

fn allowed(authority: &Authority, bind_host: &str) -> bool {
    if authority.as_str().contains('@') {
        return false;
    }
    let Ok(bind) = bind_host.parse::<IpAddr>() else {
        return false;
    };
    let host = authority.host();
    // http::Authority accepts non-numeric port suffixes; port() returns None
    // for those, so absence from that accessor is not proof of a missing port.
    let suffix = &authority.as_str()[host.len()..];
    if !suffix.is_empty()
        && suffix
            .strip_prefix(':')
            .and_then(|port| port.parse::<u16>().ok())
            .is_none()
    {
        return false;
    }
    if host.eq_ignore_ascii_case("localhost") {
        return bind.is_loopback();
    }
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .is_ok_and(|address| address == bind || bind.is_unspecified())
}

pub(super) async fn validate(
    State(bind_host): State<String>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let mut hosts = request.headers().get_all(header::HOST).iter();
    let host = hosts.next();
    if hosts.next().is_some() {
        return Err(StatusCode::FORBIDDEN);
    }
    if let Some(host) = host {
        let authority = host
            .to_str()
            .ok()
            .and_then(|host| host.parse::<Authority>().ok())
            .ok_or(StatusCode::FORBIDDEN)?;
        if !allowed(&authority, &bind_host) {
            return Err(StatusCode::FORBIDDEN);
        }
    }
    // Check HTTP/2 :authority and absolute-form requests as well as Host.
    if let Some(authority) = request.uri().authority() {
        if !allowed(authority, &bind_host) {
            return Err(StatusCode::FORBIDDEN);
        }
    } else if host.is_none() && request.version() != Version::HTTP_10 {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(next.run(request).await)
}
