glenley-agent: lets you view and control this computer from your other devices.

Set up (once):

    glenley-agent setup --server https://YOUR-CARSON-SERVER

This pairs the computer with your account (a browser window opens; sign in and click
"Add device"), asks for the permissions it needs, and starts glenley automatically at login.

Useful commands:

    glenley-agent status              pairing, key id, access mode
    glenley-agent service status      is the background service running?
    glenley-agent unattended on --pin allow connections without clicking Allow (PIN-protected)
    glenley-agent service uninstall   stop starting at login
    glenley-agent unpair              forget this computer's identity

macOS: grant "Screen & System Audio Recording" and "Accessibility" to glenley-agent in
System Settings > Privacy & Security. Downloaded (not Homebrew) copies: the first time,
right-click glenley-agent > Open, because it is not notarized (that needs a paid Apple account).
After an update, macOS may ask for the permissions again.

Windows: SmartScreen may warn about an unsigned app the first time: More info > Run anyway.

OpenH264 Video Codec provided by Cisco Systems, Inc. (downloaded from Cisco during setup when
needed; glenley does not include it).
