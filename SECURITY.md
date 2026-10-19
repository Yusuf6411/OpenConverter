# Security

Please report vulnerabilities privately through GitHub security advisories on this repository.

OpenConverter runs offline and treats every input file as hostile: decoders are bounded in memory and time and are meant to run in isolated worker processes (see docs/MASTER_PROMPT.md §16).
