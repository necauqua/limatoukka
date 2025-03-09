
## Command structure
Commands are words that end with `~`.\
So something like `left~` is a command.\
You can have multiple commands in a single message, for example `left~ up~` will run both commands at the same time.
If you want to run a command _after_ another command finishes, you can separate command groups with `|`.

For example, `left~ up~ | right~ down~` will run `left~ up~` and then `right~ down~`.
Non-command text is ignored, so `hello left~ xdd up~ lol | poggies right~ down~` is equivalent.

Certain commands can have arguments separated with `:`, for example `slot:2~` or `mouse:960:540~`.\

If the message starts with a `.` or is a reply all commands are ignored.
