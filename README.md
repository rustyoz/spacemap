# Spacemap

Live disk usage map for Linux. Each physical disk is a row; partitions, free space, and file/directory sizes are nested boxes in the WinDirStat / QDirStat style.

A background daemon walks mounted filesystems, keeps the tree on disk, and watches for changes so the map stays current.

## Run

```bash
# GUI (starts an in-process scan if the daemon is not running)
cargo run -p spacemap --release

# Long-running scanner
cargo run -p spacemapd --release
```

Install and enable the user daemon:

```bash
cargo install --path crates/spacemapd
cargo install --path crates/spacemap
mkdir -p ~/.config/systemd/user
cp packaging/spacemapd.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now spacemapd.service
```

Cached trees live in `~/.local/share/spacemap/`. The daemon socket is `$XDG_RUNTIME_DIR/spacemap.sock`.

## Use

- One row per physical disk (SSD / HDD / USB)
- Partition boxes are sized by capacity
- Hatched tiles are free space
- Colour follows file type (video, images, archives, code, …)
- Click to select, double-click a folder to zoom, Esc to zoom out, Open to reveal in the file manager
