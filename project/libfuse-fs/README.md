# FUSE Filesystem Library 

A ready-to-use filesystem library based on FUSE (Filesystem in Userspace). This library provides implementations for various filesystem types and makes it easy to develop custom filesystems.

Features:
- Asynchronous I/O support
- Overlay filesystem implementation
- Passthrough filesystem support
- Easy-to-use API for custom filesystem development


### Try
```bash
cargo test --package libfuse-fs --lib -- overlayfs::async_io::tests::test_a_ovlfs --exact --nocapture --ignored > test.log 2>&1
cargo test --package libfuse-fs --lib -- passthrough::tests::test_passthrough --exact --nocapture --ignored 
```
![alt text](image.png)