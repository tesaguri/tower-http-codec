use std::io::{stdout, Write};
use std::pin::Pin;

use futures::stream::{self, TryStreamExt};
use http::{Request, Uri};
use http_body::Body as _;
use hyper::{Body, Client};
use tower::{Service, ServiceBuilder};

#[tokio::main]
async fn main() {
    let mut client = ServiceBuilder::new()
        .layer(tower_http_codec::DecodeLayer::new())
        .service(Client::new());
    let req = Request::get(Uri::from_static("http://httpbin.org/gzip"))
        .body(Body::empty())
        .unwrap();
    let mut body = client.call(req).await.unwrap();
    let mut stdout = stdout();
    stream::poll_fn(|cx| Pin::new(&mut body).poll_data(cx))
        .try_for_each(|chunk| {
            stdout.write(&chunk).unwrap();
            async { Ok(()) }
        })
        .await
        .unwrap();
}
