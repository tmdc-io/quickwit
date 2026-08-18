some of the assets containing in this directory are test certificates and corresponding private key.
It's not unusual for automatic scanners to pick them up and warn about leaked private keys. These
keys are not meant to be private, so if that happen, feel free to ignore the messages.

Private keys (`*.key`) are gitignored and generated locally by running:

```bash
make test-tls-certs
```

from the `quickwit/` directory (also run automatically before `make test-all`).
