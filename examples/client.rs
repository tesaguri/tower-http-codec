use std::convert::TryInto;
use std::env;
use std::io::{stdout, Write};
use std::pin::Pin;

use futures::stream::{self, TryStreamExt};
use http::{Request, Uri};
use http_body::Body as _;
use hyper::{Body, Client};
use tower::{Service, ServiceBuilder};

#[tokio::main]
async fn main() {
    let uri: Uri = env::args()
        .nth(1)
        .expect("missing URI argument")
        .try_into()
        .unwrap();

    let mut client = ServiceBuilder::new()
        .layer(tower_http_compression::DecodeLayer::new())
        .service(Client::new());

    let req = Request::get(uri).body(Body::empty()).unwrap();
    let mut body = client.call(req).await.unwrap();

    let mut stdout = stdout();
    stream::poll_fn(|cx| Pin::new(&mut body).poll_data(cx))
        .try_for_each(|chunk| {
            stdout.write_all(&chunk).unwrap();
            async { Ok(()) }
        })
        .await
        .unwrap();
}
