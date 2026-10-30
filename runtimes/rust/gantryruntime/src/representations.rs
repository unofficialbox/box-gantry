//! Box's file representations: [`Client::representation_content`] follows the
//! protocol from an item request to the representation's bytes.

use std::time::Duration;

use crate::{response_bytes, response_stream, Client, Error, Request, Stream};

/// How long [`Client::representation_content`] waits for Box to generate a
/// representation unless the caller passes its own bound.
pub const DEFAULT_REPRESENTATION_WAIT: Duration = Duration::from_secs(300);

impl Client {
    /// Run Box's representation protocol for the item `request` fetches, which
    /// already asks for `field` and carries the `X-Rep-Hints` header: take the
    /// first representation Box offers under `field`, poll its info URL while
    /// Box generates it (`pending`, `none`), then download `asset_path` from its
    /// content URL template (`""` for a single-file representation such as a
    /// PDF or extracted text). A representation Box fails to generate
    /// (`error`) is an error, as is waiting longer than `max_wait`
    /// ([`DEFAULT_REPRESENTATION_WAIT`] when `None`).
    pub async fn representation_content(
        &self,
        request: Request,
        field: &str,
        asset_path: &str,
        max_wait: Option<Duration>,
    ) -> Result<Stream, Error> {
        let wait = max_wait.unwrap_or(DEFAULT_REPRESENTATION_WAIT);
        tokio::time::timeout(wait, self.follow_representation(request, field, asset_path))
            .await
            .unwrap_or_else(|_| {
                Err(Error::new(
                    "box: timed out waiting for Box to generate the representation",
                ))
            })
    }

    async fn follow_representation(
        &self,
        request: Request,
        field: &str,
        asset_path: &str,
    ) -> Result<Stream, Error> {
        let item: serde_json::Value =
            serde_json::from_slice(&response_bytes(&self.fetch(request).await?)?)?;
        let mut rep = item[field]["entries"].get(0).cloned().ok_or_else(|| {
            Error::new("box: no representation matches the X-Rep-Hints requested")
        })?;
        let text = |value: &serde_json::Value, a: &str, b: &str| {
            value[a][b].as_str().unwrap_or_default().to_string()
        };
        let name = rep["representation"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let mut delay = Duration::from_millis(250);
        loop {
            match text(&rep, "status", "state").as_str() {
                "success" | "viewable" => break,
                "pending" => tokio::time::sleep(delay).await,
                "none" => {}
                "error" => {
                    return Err(Error::new(format!(
                        "box: Box could not generate the {name} representation ({})",
                        text(&rep, "status", "code")
                    )))
                }
                state => {
                    return Err(Error::new(format!(
                        "box: unknown representation state {state}"
                    )))
                }
            }
            delay = (delay * 2).min(Duration::from_secs(5));
            let info = text(&rep, "info", "url");
            if info.is_empty() {
                return Err(Error::new(format!(
                    "box: the {name} representation has no info URL to poll"
                )));
            }
            let polled: serde_json::Value = serde_json::from_slice(&response_bytes(
                &self.fetch(self.new_request("GET", &info)).await?,
            )?)?;
            rep["status"] = polled["status"].clone();
            if !text(&polled, "content", "url_template").is_empty() {
                rep["content"] = polled["content"].clone();
            }
        }
        let template = text(&rep, "content", "url_template");
        if template.is_empty() {
            return Err(Error::new(format!(
                "box: the {name} representation has no content URL"
            )));
        }
        let url = template.replacen("{+asset_path}", asset_path, 1);
        Ok(response_stream(
            &self.fetch(self.new_request("GET", &url)).await?,
        ))
    }
}
