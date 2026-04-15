# /hex-shutdown — Close Session

Quick session close. Lighter than checkpoint — no handoff file, just final persistence.

## Steps

1. **Quick distill:** Scan the last few messages for anything not yet persisted. Write it now.

2. **Update landings:** Update status on any landing items that changed during this session.

3. **Report:** "Session closed."
