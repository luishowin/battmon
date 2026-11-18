# Building the RPM

```
packaging/rpm/make-vendor-tarball.sh          # both sources, into ~/rpmbuild/SOURCES
rpmbuild -ba packaging/rpm/battmon.spec       # → ~/rpmbuild/RPMS/x86_64/battmon-<version>-1.fc45.x86_64.rpm
sudo dnf install ~/rpmbuild/RPMS/x86_64/battmon-<version>-1.fc45.x86_64.rpm
systemctl --user enable --now battmond        # start the sampler for your user
```

- **The source tarball is `git archive HEAD`.** Commit before building; the
  script warns if the tree is dirty.
- **The build is offline.** The vendor tarball carries every crate in
  `Cargo.lock`, together with the source replacement that points cargo at them.
- **`%check` runs the whole test suite.** The tests paint into cairo image
  surfaces, so they do not need a display.
- **The package installs these files:**
  - the `battmon` window and `battmond` sampler in `%{_bindir}`
  - the desktop file and icon
  - the systemd user unit in `%{_userunitdir}`, with `ExecStart` rewritten to
    the packaged binary

  It changes nothing per user. Enable the unit yourself, as above.
- **A development install in `~/.local` takes precedence over the RPM's
  files** for the binaries, the unit, the desktop file and the icon. Remove
  it with `scripts/install.sh`'s uninstall steps in README.md before relying
  on the package.
