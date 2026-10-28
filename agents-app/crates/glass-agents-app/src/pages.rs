//! The app's own few pages, before the web app can show: where the
//! person's Glasshouse Agents is, signing in, and waiting for the browser.
//! Plain forms posted to the `agents:` scheme, no script, styled by the web
//! app's own stylesheet (`/app.css`, as the front's sign-in page is).

/// `text` safe inside HTML text and attribute values.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{title} · Glasshouse Agents</title><link rel=\"stylesheet\" href=\"/app.css\">\
         </head><body><main class=\"page app-page\">{body}</main></body></html>",
        title = escape(title),
    )
}

fn note(note: Option<&str>) -> String {
    note.map(|n| format!("<p class=\"notice bad\" role=\"alert\">{}</p>", escape(n)))
        .unwrap_or_default()
}

/// The first page: the company's Glasshouse Agents address. IT usually
/// sets it (`--url`, or the app's settings file), so most people never see
/// this.
pub fn address(said: Option<&str>) -> String {
    page(
        "Where are your agents?",
        &format!(
            "<h1>Where are your agents?</h1>\
             <p class=\"sub\">Your company's Glasshouse Agents address. Your IT team can \
             tell you, or set it for you.</p>{note}\
             <form method=\"post\" action=\"/_app/address\">\
             <label for=\"url\">Address</label>\
             <input id=\"url\" name=\"url\" type=\"url\" required autocomplete=\"url\" \
             placeholder=\"https://agents.yourcompany.com\">\
             <button class=\"btn primary big\">Continue</button></form>",
            note = note(said),
        ),
    )
}

/// Signing in: the browser does the company's sign-in, so the app never
/// sees the person's password; it keeps only a token, in the keychain.
pub fn sign_in(url: &str, said: Option<&str>) -> String {
    page(
        "Sign in",
        &format!(
            "<h1>Sign in to Glasshouse Agents</h1>\
             <p class=\"sub\">At {url}. Your browser opens your company's sign-in; this app \
             keeps your sign-in in this computer's keychain, for 30 days or until you sign \
             out.</p>{note}\
             <form method=\"post\" action=\"/_app/sign-in\">\
             <button class=\"btn primary big\">Sign in with your browser</button></form>\
             <form method=\"post\" action=\"/_app/address\">\
             <input type=\"hidden\" name=\"url\" value=\"\">\
             <button class=\"btn link\">Use another address</button></form>",
            url = escape(url),
            note = note(said),
        ),
    )
}

/// While the person signs in in their browser.
pub fn waiting(link: &str) -> String {
    page(
        "Finish signing in",
        &format!(
            "<h1>Finish signing in in your browser</h1>\
             <p class=\"sub\">Your browser opened your company's sign-in. Allow the \
             Glasshouse Agents app there, and this window carries on by itself.</p>\
             <p class=\"sub\">If it didn't open, go to this address in your browser:</p>\
             <p><code class=\"link\">{link}</code></p>\
             <form method=\"post\" action=\"/_app/sign-in\">\
             <button class=\"btn secondary\">Start again</button></form>",
            link = escape(link),
        ),
    )
}

/// Built without the web app: says how to build it.
pub fn unbuilt() -> String {
    page(
        "Not built",
        "<h1>This app was built without its web app</h1>\
         <p class=\"sub\">Run <code>make agents-app</code>, which builds the web app first.</p>",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adr0071_what_a_page_shows_is_never_markup() {
        let shown = sign_in(
            "https://agents.example.com/<script>",
            Some("it said \"no\" & <b>"),
        );
        assert!(shown.contains("https://agents.example.com/&lt;script&gt;"));
        assert!(shown.contains("it said &quot;no&quot; &amp; &lt;b&gt;"));
        assert!(!shown.contains("<script>"));
        assert!(waiting("http://x/login/cli?a=1&b=<2>").contains("a=1&amp;b=&lt;2&gt;"));
        assert!(address(None).contains("action=\"/_app/address\""));
        assert!(!address(None).contains("role=\"alert\""));
    }
}
