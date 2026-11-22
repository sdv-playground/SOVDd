# Example configurations

Example configurations for the standalone `sovdd` server and the simulated
ECU (`example-ecu`): single ECU on vcan0, multi-ECU gateways, and DID
definitions. They are used by the docs and the end-to-end tests
(`crates/sovd-tests`).

None of them is read by an embedding application. The server's default
config is `config/sovd.toml`; runnable test networks live in `simulations/`.

Product and ECU names in these files (e.g. "Vortex Motors") are invented.
