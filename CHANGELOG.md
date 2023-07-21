# Changelog

All notable changes to this project will be documented in this file.

# 0.2.1 (21. July, 2023)
- **fix** FusedStream requirement was not part of the release (Other changes were not breaking) (0.2.0 will be yanked)

# 0.2.0 (21. July, 2023)
- **breaking** Inner Streams must implement FusedStream instead of wrapping them again
- **add** Introduce WeakStreamBroadcast (a3c96f1718869330ba7b1801598f81cb1c4e833e)

# 0.1.1 (20. July, 2023)
- **fix** Weakup other Streams if they polled the shared stream resulting in Poll::Pending 

# 0.1.0 (20. July, 2023)
- Initial release