# Changelog

All notable changes to Aggligator will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## 0.9.7 - 2025-08-19
### Added
- better integration with tracing crate:
  aggligator now uses spans for connections and transport tasks

## 0.9.6 - 2025-06-22
### Added
- transport: public type aliases for boxed types

## 0.9.5 - 2025-03-08
### Fixed
- panic when transport handle is dropped

## 0.9.4 - 2025-02-19
### Changed
- update dependencies

## 0.9.3 - 2025-01-23
### Changed
- documentation

## 0.9.2 - 2025-01-23
### Fixed
- documentation

## 0.9.1 - 2025-01-23
### Fixed
- documentation

## 0.9.0 - 2025-01-23
### Added
- WebAssembly support
- JavaScript runtime environment support, enabled by `js` crate feature
### Changed
- move transport module from aggligator-util into aggligator crate

## 0.8.3 - 2023-11-02
### Changed
- shorten log messages

## 0.8.2 - 2023-09-06
### Changed
- update dependencies

## 0.8.1 - 2023-02-13
### Changed
- move repetitve debug messages to trace level

## 0.8.0 - 2023-02-13
### Changed
- harmonize change notifications

## 0.7.1 - 2023-02-10
### Added
- configuration option `link_test_data_limit` to limit amount of test data
  for link testing

## 0.7.0 - 2023-02-08
### Added
- improved error reporting

## 0.6.0 - 2023-02-07
### Added
- configuration option to disconnect on server id mismatch

## 0.5.1 - 2023-02-07
### Changed
- reduce debug logging

## 0.5.0 - 2023-02-06
### Added
- link blocking
### Changed
- protocol version 4

## 0.4.0 - 2023-02-06
### Added
- data integrity checking for IO-based links
- publish reason why a link is currently not working
### Changed
- protocol version 3
- optimize resend queue handling

## 0.3.3 - 2023-02-05
### Added
- statistics for number of link hangs
### Changed
- optimize unconfirmed link handling
- optimize resend queue handling

## 0.3.2 - 2023-02-02
### Added
- `link_max_ping` configuration option to only use links
  that satisfy the ping requirement
- control methods to mark links and stats as seen
### Fixed
- optimize resending
- race condition when testing link
- do not wait for flush of unconfirmed links
- do not use crypto random number generator when unnecessary

## 0.3.1
### Added
- `control::links_update` and `control::stats_update` methods

## 0.3.0
### Added
- convert error types into std::io::Error
### Changed
- remove unnecessary async on some functions

## 0.2.2
### Added
- re-exports for easier use

## 0.2.1
### Changed
- use cryptographic random number generator for connection id

## 0.2.0
### Added
- encrypt connection id using a shared secret exchanged using Diffie-Helmann;
  this hinders an eavesdropper to take over a connection by spoofing the
  connection id
### Changed
- increse buffer sizes and adjust timeouts for better performance over high latency
  links
### Fixed
- link disconnect reason for link filter rejection

## 0.1.1
### Fixed
- make `dump` non-default feature

## 0.1.0
### Added
- initial release

