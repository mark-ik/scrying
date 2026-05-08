//! `NSHTTPCookie` ↔ [`Cookie`] translation helpers used by the
//! producer's cookie-store API (`request_all_cookies` / `set_cookie`
//! / `delete_cookie`). Pure functions — no producer state.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{
    NSDate, NSDictionary, NSHTTPCookie, NSHTTPCookieDomain, NSHTTPCookieExpires,
    NSHTTPCookieName, NSHTTPCookiePath, NSHTTPCookieSecure, NSHTTPCookieValue, NSString,
};

use crate::Cookie;

/// Convert an `NSHTTPCookie` into our [`Cookie`] payload. All
/// translations happen on the main thread inside the cookie-store
/// completion block.
pub(super) fn cookie_from_ns(ns_cookie: &NSHTTPCookie) -> Cookie {
    let expires_at = ns_cookie.expiresDate().map(|d| d.timeIntervalSince1970());
    Cookie {
        name: ns_cookie.name().to_string(),
        value: ns_cookie.value().to_string(),
        domain: ns_cookie.domain().to_string(),
        path: ns_cookie.path().to_string(),
        expires_at,
        is_secure: ns_cookie.isSecure(),
        is_http_only: ns_cookie.isHTTPOnly(),
    }
}

/// Build an `NSHTTPCookie` from our [`Cookie`] payload via
/// `NSHTTPCookie::cookieWithProperties:`. Returns `None` if the
/// property dictionary fails Apple's parser (typically: missing /
/// malformed name / value / domain / path).
///
/// Booleans go in as `"TRUE"` / `"FALSE"` `NSString`s — Apple's
/// docs explicitly support that, and it lets us avoid pulling
/// `objc2_foundation/NSNumber` (not in default features) just for
/// this. Expiry, when present, uses `NSDate` directly which
/// `NSHTTPCookie::cookieWithProperties:` accepts.
pub(super) fn ns_cookie_from(cookie: &Cookie) -> Option<Retained<NSHTTPCookie>> {
    let name_ns = NSString::from_str(&cookie.name);
    let value_ns = NSString::from_str(&cookie.value);
    let domain_ns = NSString::from_str(&cookie.domain);
    let path_ns = NSString::from_str(&cookie.path);
    let secure_ns = NSString::from_str(if cookie.is_secure { "TRUE" } else { "FALSE" });

    let mut keys: Vec<&NSString> = vec![
        unsafe { NSHTTPCookieName },
        unsafe { NSHTTPCookieValue },
        unsafe { NSHTTPCookieDomain },
        unsafe { NSHTTPCookiePath },
        unsafe { NSHTTPCookieSecure },
    ];
    // Hold the strong refs alive so the `&AnyObject` slice we pass
    // to `NSDictionary::from_slices` stays valid until the dict is
    // built.
    let mut values: Vec<Retained<AnyObject>> = vec![
        unsafe { Retained::cast_unchecked(name_ns) },
        unsafe { Retained::cast_unchecked(value_ns) },
        unsafe { Retained::cast_unchecked(domain_ns) },
        unsafe { Retained::cast_unchecked(path_ns) },
        unsafe { Retained::cast_unchecked(secure_ns) },
    ];
    if let Some(unix_ts) = cookie.expires_at {
        let date = NSDate::dateWithTimeIntervalSince1970(unix_ts);
        keys.push(unsafe { NSHTTPCookieExpires });
        values.push(unsafe { Retained::cast_unchecked(date) });
    }

    let value_refs: Vec<&AnyObject> = values.iter().map(|v| &**v).collect();
    let dict = NSDictionary::from_slices(&keys, &value_refs);
    unsafe { NSHTTPCookie::cookieWithProperties(&dict) }
}
