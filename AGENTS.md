# Project scope

cc-pay is an API library for other projects to import and reference. Keep code,
public interfaces and documentation focused on library integration. The host
application owns entry points and application configuration.

- Core `cc-pay` must not depend on a browser runtime, Node scripts or optional
  adapter crates. Integrate external behavior through traits.
- The host owns runtime setup, browser launch, proxy configuration, credentials,
  logging, persistence and UI. Accept these explicitly through APIs.
- Preserve payment behavior when refactoring: exact amount checks, persistent
  atomic claims, one debit dispatch and uncertain-result protection.
- Do not validate changes by submitting real payments. Use mock transports and
  local browser fixtures. Distinguish these from real provider acceptance.
- Do not log credentials, session material, signed payment URLs or raw responses.

## Documentation

Describe the current implementation, public APIs, configuration and usage. Base
examples and behavioral claims on the checked-in code. Use direct statements of
functionality and keep Chinese and English documentation consistent.
