use futures::{SinkExt, StreamExt};
use reqwasm::websocket::*;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

const ECHO_SERVER_URL: &str = env!("ECHO_SERVER_URL");

#[wasm_bindgen_test]
async fn websocket_works() {
    let mut ws = WebSocket::open(ECHO_SERVER_URL).unwrap();

    ws.send(Message::Text("test".to_string())).await.unwrap();

    // ignore first message
    // the echo-server used sends it's info in the first message
    let _ = ws.next().await;
    assert_eq!(
        ws.next().await.unwrap().unwrap(),
        Message::Text("test".to_string())
    )
}
