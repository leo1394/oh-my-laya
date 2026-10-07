use axum::{body::Body, http::{StatusCode, Uri}, response::{IntoResponse, Response}};

#[derive(rust_embed::RustEmbed)]
#[folder = "../../web/dist/"]
struct Assets;

pub async fn serve(uri: Uri) -> Response {
    let path=uri.path().trim_start_matches('/');
    let name=if path.is_empty() {"index.html"} else {path};
    match Assets::get(name) {
        Some(asset)=> {
            let mime=mime_guess::from_path(name).first_or_octet_stream();
            Response::builder().header("content-type",mime.as_ref())
                .header("x-content-type-options","nosniff")
                .header("referrer-policy","no-referrer")
                .header("content-security-policy","default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'")
                .header("cache-control","no-cache")
                .body(Body::from(asset.data.into_owned())).unwrap()
        }
        None=>(StatusCode::NOT_FOUND,"Not found").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_case_study_uses_current_chinese_label() {
        let scripts:Vec<_>=Assets::iter().filter(|name|name.ends_with(".js")).collect();
        assert!(!scripts.is_empty(), "build the dashboard before testing the bundle");
        let mut has_current_label=false;
        for name in scripts {
            let asset=Assets::get(&name).unwrap();
            let text=std::str::from_utf8(&asset.data).unwrap();
            assert!(!text.contains("案例库"), "stale dashboard bundle: {name}");
            has_current_label|=text.contains("案例学习");
        }
        assert!(has_current_label, "current dashboard label missing from bundle");
    }
}
