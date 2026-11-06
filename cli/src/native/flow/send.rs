//! The run loop: each request of a flow is sent once, in order.

use async_trait::async_trait;

use super::*;

/// Sends one prepared request and reports what came back.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait Sender: Send {
    async fn send(&mut self, req: &PreparedRequest) -> RequestResult;
}

/// Send each request in order, filling references from the answers so far,
/// and stop at the first one that does not come back as recorded. `check`
/// runs on every request after its references are filled and may refuse the
/// run. A request is never sent twice, a write included.
pub async fn run_requests(
    requests: &[PreparedRequest],
    sender: &mut dyn Sender,
    check: &(dyn Fn(&PreparedRequest) -> Result<(), String> + Sync),
) -> Result<Vec<RequestResult>, String> {
    let mut results = Vec::new();
    let mut bodies: HashMap<String, Value> = HashMap::new();
    for prepared in requests {
        let req = match prepared.resolve(&bodies) {
            Ok(r) => r,
            Err(e) => {
                results.push(RequestResult::fail(
                    prepared,
                    Instant::now(),
                    Failure::drift(e),
                ));
                break;
            }
        };
        check(&req)?;
        let result = sender.send(&req).await;
        if let Some(body) = result.body.as_ref().filter(|_| result.ok) {
            bodies.insert(req.id.clone(), body.clone());
        }
        let ok = result.ok;
        results.push(result);
        if !ok {
            break;
        }
    }
    Ok(results)
}
