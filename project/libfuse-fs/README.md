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

## Integration Tests (OverlayFS & PassthroughFS)

We provide an integration script that mounts overlayfs and passthroughfs examples, then runs:

* IOR for basic POSIX read/write validation.
* fio for sequential write/read and random mixed workloads.

### Run Locally
Requirements: `fio`, `ior`, `fuse3` (providing `fusermount3`), Rust toolchain.

```bash
cd project/libfuse-fs
./tests/integration_test.sh
```

Set `WORKDIR=/custom/tmp` to control temporary directory. Logs are stored under `$WORKDIR/logs`.

### GitHub Actions
Workflow file: `.github/workflows/libfuse-fs-integration.yml` (runs on PR touching this crate).

### Examples
Example binaries used by the integration tests:
```bash
cargo run --example overlayfs_example -- \
	--mountpoint /tmp/ovl_mnt --upperdir /tmp/ovl_upper \
	--lowerdir /usr --lowerdir /bin

cargo run --example passthrough_example -- \
	--mountpoint /tmp/pt_mnt --rootdir /var/tmp
```

### Contributing
All commits must be signed (`git commit -s`) and GPG signed (`-S`) per project policy.
