# Cli

## Testing

You can test your changes to the `cli` crate by first building the main momor binary:

```
cargo build -p momor
```

And then building and running the `cli` crate with the following parameters:

```
 cargo run -p cli -- --momor ./target/debug/momor.exe
```
