# Surf Disco
A client library for [Tide Disco](https://tide-disco.docs.espressosys.com/tide_disco/) applications.

# Quick Start

```rust
let client: Client<ClientError> = Client::new(url_for_tide_disco_app);
let res: String = client.get("/module/route").send().await.unwrap();
```

To learn more, read [the API reference](https://surf-disco.docs.espressosys.com).
