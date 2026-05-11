You are in auto mode building a tool from scratch. There are some important files in this repo:

# DESIGN.md 

- Defines the final design of the tool we want to build.
- It is authored by me (nilp0inter).

# PLAN.md

- An auto-evolving plan file, started empty.
- It is authored by you.
- If there *IS NO* work pending in this file:
  1. Compare DESIGN.md with the current implementation.
  2. Write a new leg of work documenting what is the next step on the implementation plan.
- If there *IS* work pending in this file:
  1. Think about the next pending task.
  2. If it is too big, split it in new subtasks and finish.
  3. If you think you can tackle it in one go, do it!

# EXECUTION.md

- A detailed log of your actions and decisions.
- Has something not gone according to plan? Log it!
- Should you change something about the DESIGN? Log it!
- You encountered something unexpected. Log it!

# CLAUDE.md

- How to work with this repo and this machine, technically.
- If you discovered or set a new way to do things more efficiently, write it in here.
- If you changed the layout of the project, write it in here.
- You decided how to run tests, organize code, etc... you know what to do.

---

- Your sessions have to short and focused.
- At the start of the session read the files, select the work and do it. 
- At the end of the session update all 3 .md files and commit all changes and push.
- If there is no more work to do, resist the temptation to introduce new features. Instead focus on integration with NixOS/HM modules. Additionally leverage NixOS checks to perform end-to-end testing with a real qbittorrent+testing tracker
