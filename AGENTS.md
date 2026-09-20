# AGENTS.md

These are the rules for an agent that does work in this repository.
The rules apply to all changes.
They also apply to a change that you start without a request.

## Who writes a commit

- A human is the writer of each commit. An agent is not.
- Set `git config user.name` and `git config user.email` one time.
  Then do not change them.
- Do not add your name to a commit message, a pull request, or a review
  comment.
- Do not add a `Co-Authored-By` line.
- Do not add a link to your session.
- Do not add a footer that says that a tool made the text.
- Make these changes in the configuration of the tool.
  Do not remove the text manually each time.
- Reason: a commit shows that a human read the code.
  That human answers questions about the code six months later.
  An agent cannot do this.

## How to write

- Write all text for this repository in ASD-STE100 Simplified Technical
  English.
  This includes the source code, the comments, the documentation, the commit
  messages, the examples, and the pull requests.
- Use short sentences.
  Use the active voice.
  Use the present tense.
  Write one instruction in one sentence.
- Do not use an em-dash.
- Do not use an emoji.

## Commits

- Each commit has one change only.
- Split a feature into many commits.
  One commit does one step of the feature.
  Do not put a full feature into one commit.
- Reason: a reader examines one step at a time.
  A reader also removes one step and keeps the other steps.
  A commit that holds a full feature stops both of these actions.
  Six months later, a reader finds the one step that caused a fault.
  That reader cannot find it in a commit that changed twenty files.
- Put the code and its tests in the same commit.
- Put the documentation in a different commit.
- If you change a name in many files, put that change alone in one commit.
  Do not change what the code does in that commit.
- Write the subject line in the present tense.
  The subject line tells what the change does.
- Do not put a version number in the subject line.
- A commit does not change the version.
- Do not open a pull request if the human does not ask for it.

### An example of granular commits

These commits are correct.
Each commit does one step.
The steps are in the same feature group.

    feat: restructure example.ts for the new parser
    feat: add the token reader to example.ts
    feat: connect the parser to the command line

This commit is not correct.
It holds the full feature.

    feat: integrate the full parser feature

## How to work

- Run the code.
  Do not only say that the code will operate correctly.
- Tell the human when the results do not agree with your statement.
- Read the open issues before you add a builtin or new syntax.
- Some issues are for a person who helps this project for the first time.
  Do not do the work in those issues.

## What to measure

- Measure the thing that you tell the human.
  Do not measure something near it.
- Reason: a size is not a state.
  A watcher looked at the size of a directory and said that a download was
  complete.
  One gigabyte was still to come, because a file that nothing used was in the
  same directory.
- Reason: a search for some names is not a search for all of them.
  A search for three program names said that no task was running.
  A watcher was running, and its name was not one of the three.
- Give the number that you can prove.
  Do not give a number that you calculated from a part.

## What a test can hold on to

- A test asks the code a question.
  Do not let it ask the example configuration a question.
- Build the state that a test needs inside the test.
  Do not read it out of the file that the author edits.
- Reason: the example is a working book, and every value in it is a choice
  that the author makes again.
  A test that read the action mode from it failed when the author changed the
  mode.
  A test that counted the entries with no voice failed when the last of them
  received one.
  A test that asked for the host by name failed when the host changed for one
  render.
- Reason: a test can also pass for the wrong reason.
  One test wrote the word "drop" into a file that already said "drop", so it
  tested nothing, and nobody could see this.
- Match the shape of the file, not one form of it.
  A helper looked for one space before an equals sign.
  An entry with more spaces kept its voice, and the render that the test
  refuses went through.

## What to keep

- Look in a directory before you delete it.
  The size of a directory is not the contents of it.
- Put each thing that the human chooses into the repository.
  Do not keep it only in a working directory.
