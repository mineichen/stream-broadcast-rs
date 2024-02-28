# Changelog

All notable changes to this project will be documented in this file.

# 0.2.3 (22. February, 2024)
- **bugfix** Cloned `StreamBroadcast` and `WeakStreamBroadcast` now inherit the position of their origin, so they both see the same messages.
- **add** Implement `std::fmt::Debug` for `StreamBroadcast` and `WeakStreamBroadcast`
- Yanked for incompatible clone method. 

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