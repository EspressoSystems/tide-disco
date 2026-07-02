# Tide Disco
_Discoverability support for [Tide](https://github.com/http-rs/tide)_

We say a system is _discoverable_ if guesses and mistakes regarding
usage are rewarded with relevant documentation and assistance at
producing correct requests. To offer this capability in a practical
way, it is helpful to specify the API in data files, rather than code,
so that all relevant text can be edited in one concise readable
specification.

Leverages TOML to specify
- Routes with typed parameters
- Route documentation
- Route error messages
- General documentation

## Goals

- Context-sensitive help
- Spelling suggestions
- Reference documentation assembled from route documentation
- Forms and other user interfaces to aid in the construction of correct inputs
- Localization
- Novice and expert help
- Flexible route parsing, e.g. named parameters rather than positional parameters
- API fuzz testing automation based on parameter types

## Future

WebSocket support
Runtime control over logging
