---
allowed-tools: Read, Task, Bash(git log:*), Bash(git add:*), Bash(git commit:*), Bash(git status:*), Edit, Grep
description: Execute todo task with verification
argument-hint: @todo/filename.md
---

# Execute todo task with verification

Execute task from todo file with status tracking and result documentation.

## Input data

**Todo file:** $ARGUMENTS

## Pipeline

### Stage 1: Read and validate task

Read the todo file at $ARGUMENTS

If file does not exist:
```
ERROR: Todo file not found at path $ARGUMENTS
STOPPING EXECUTION.
```

Extract:
- Task title and description
- Requirements and plan (if present)
- Current status

### Stage 2: Check and update status

Check current status from the content read in Stage 1.

If status is already "in_progress":
- Ask user for confirmation to continue (task may be running elsewhere)
- If user declines → stop execution

Update frontmatter:
```yaml
status: in_progress
updated: [current date YYYY-MM-DD]
```

### Stage 3: Execute task

Analyze task requirements and execute:

1. Understand what needs to be done from the todo file
2. Follow the implementation plan if provided
3. Create or modify files as needed
4. **CRITICAL: DO NOT change status to 'completed' yet**

### Stage 4: Document result

Add to end of todo file:

```markdown
## Execution result

**Date:** [timestamp]
**Created files:**
- `path/to/file` - [brief description]

**Modified files:**
- `path/to/file` - [what changed]

**Summary:**
[What was achieved, any notes]
```

### Stage 5: Validate and finalize

**Pre-final checks:**
- Todo file exists and has result section
- All created files exist
- No blocking errors

**Update frontmatter:**
```yaml
status: completed
updated: [current date]
completed: [current date]
```

**Create git commit:**
```bash
git add "$ARGUMENTS"
git add [created/modified files]
git commit -m "feat: completed task [todo_title]

Todo reference: $ARGUMENTS"
```

## Final report

```markdown
# Todo task completed

**File:** $ARGUMENTS
**Status:** completed

## Created files
- [list of files]

## Git commit
[commit hash and message]
```

## Error handling

### Task cannot be completed

1. Keep status `in_progress`
2. Add section to todo:

```markdown
## Execution problem

**Date:** [timestamp]
**Issue:** [what went wrong]
**Recommendations:** [how to fix]
```

3. Report to user

### Rollback on failure

If task cannot proceed and changes should be discarded:
```bash
git restore "$ARGUMENTS"
```

Or if partial progress should be kept, update status:
```yaml
status: draft
```

## Status transitions

```
draft → in_progress → completed
  ↑          ↓
  └──(rollback on error)
```

- `draft` - task created, work not started
- `in_progress` - task being executed
- `completed` - task finished successfully
