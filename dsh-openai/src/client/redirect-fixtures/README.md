These inert certificates and the server private key are public test fixtures.
The CA signs only the localhost/127.0.0.1 server certificate; its private key
is not part of the repository. They are valid from October 2026 to September
2126. Never use these fixtures for a real service.

The redirect tests add this CA only to a test client's root set. They retain
certificate and hostname verification and never install a system trust root.
All listeners bind to ephemeral IPv4 loopback ports and have bounded shutdown.
