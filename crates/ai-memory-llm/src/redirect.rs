//! Providers contact only the exact endpoint configured by the operator.
//!
//! Even a same-origin redirect can disclose credentials to a different handler.
//! Refuse every hop before constructing a follow-up request.

/// Refuse redirects, including rewrites within the configured origin.
pub(crate) fn no_redirects() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let target = attempt.url().origin().ascii_serialization();
        attempt.error(format!(
            "refused a redirect to {target}; set the endpoint to its final URL"
        ))
    })
}
