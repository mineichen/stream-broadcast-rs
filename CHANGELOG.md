# Changelog

All notable changes to this project will be documented in this file.

# 0.3.2
- Replace futures dependency with futures-core

# 0.3.1 (7. September, 2026)
- **add** `StreamBroadcastLossless`, a subscriber which never skips items: while it still needs
  the oldest buffered item, the input stream is not polled, so producers feeding it stall.
  Create one with `StreamBroadcastExt::broadcast_lossless` or `StreamBroadcastLossy::create_lossless`
- **add** `StreamBroadcastLossy`, a rename of `StreamBroadcast`. Create one with the new
  `StreamBroadcastExt::broadcast_lossy`, or from a lossless subscriber with `StreamBroadcastLossless::create_lossy`
- **add** `WeakStreamBroadcast::create_lossy` and `WeakStreamBroadcast::create_lossless`
- **deprecate** `StreamBroadcastExt::broadcast` in favor of `broadcast_lossy`
- **deprecate** `StreamBroadcast` type alias in favor of `StreamBroadcastLossy`
- **deprecate** `WeakStreamBroadcast::upgrade` in favor of `create_lossy`
- **bugfix** The buffer size was read back via `Vec::capacity()`, which is `usize::MAX` for
  zero-sized item types, so the cache grew unboundedly and no item was ever reported as skipped
- **bugfix** A buffer size of 0 now panics immediately with a clear message, instead of panicking
  on the first item with "attempt to calculate the remainder with a divisor of zero"
- **bugfix** A subscriber parked on a `Pending` input stream was never woken once another
  subscriber observed the stream's end, hanging forever instead of also completing
- **bugfix** `Debug` panicked with an underflow for a subscriber that had already observed the
  end of the stream
- **bugfix** `FusedStream::is_terminated` reported `true` while a lagging subscriber still had
  buffered items left to deliver, which could cause `select!`/`SelectAll` to drop them silently

# 0.3.0 (22. February, 2024)
- **add** `StreamBroadcast::re_subscribe` and `WeakStreamBroadcast::re_subscribe` provide the old behaviour of clone()
- **breaking** Remove deprecated `StreamBroadcast::weak` 
- **breaking** Clone continues on the same position as it's origin. Use re_subscribe() if you need the old behaviour

# 0.2.3 (22. February, 2024)
- **bugfix** Cloned `StreamBroadcast` and `WeakStreamBroadcast` now inherit the position of their origin, so they both see the same messages.
- **add** Implement `std::fmt::Debug` for `StreamBroadcast` and `WeakStreamBroadcast`
- Yanked for incompatible clone method. Release 0.3 instead, which contains all features of this release

# 0.2.2 (21. July, 2023)
- **deprecate** Use the more common names `downgrade` to switch from StreamBroadcast->WeakStreamBroadcast. The `weak` method became deprecated
- **add** Introduce `upgrade` to get a `Option<StreamBroadcast>` from `WeakStreamBroadcast`

# 0.2.1 (21. July, 2023)
- **fix** FusedStream requirement was not part of the release (Other changes were not breaking) (0.2.0 will be yanked)

# 0.2.0 (21. July, 2023)
- **breaking** Inner Streams must implement FusedStream instead of wrapping them again
- **add** Introduce WeakStreamBroadcast (a3c96f1718869330ba7b1801598f81cb1c4e833e)

# 0.1.1 (20. July, 2023)
- **fix** Weakup other Streams if they polled the shared stream resulting in Poll::Pending 

# 0.1.0 (20. July, 2023)
- Initial release