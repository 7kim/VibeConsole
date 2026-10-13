## tested issues and problems:
1) Configure --> Knobs = error "Operation failed; saved configuration preserved. Resume explicitly after resolving the error.  " and pauses VibeConsole
2) Configure --> piano == error "Operation failed; saved configuration preserved. Resume explicitly after resolving the error.  " and pauses VibeConsole
3) Configure --> joystick == error "Operation failed; saved configuration preserved. Resume explicitly after resolving the error.  " and pauses VibeConsole

4) Run --> Start wisprflow == VibeConsole pauses
5) Run --> repair wisprflow == VibeConsole pauses

6) Run --> Click Pad 1 arms it. 2nd click runs the app or shortcut. This means double click for first time. if paused, run. arm--> then 2nd click works. I want it armed from start always
7) If app launched through pad or whatever. 2nd click on the same pad should minimize it. Two choices for setting up application launchers, 2nd click either minimize or close
8) we need default agents path and also use the per agent path. set in settings default agent path.
9) configure --> pad --> run command in linux runs it in temux but doesnt open that tmux session
10) configure --> pad --> prompt == error "Operation failed; saved configuration preserved. Resume explicitly after resolving the error.  " and pauses VibeConsole


11) when i configure --> run command in linux i should have option to set Feedback for that action. for all actions i should have chance to set feedback so i know what is happening. For example linux command "claude". I should be notified that it went to tmux and is running. when its not running by clicking pad 8 (tmux kill-server) the feedback should go back 
12) add in settings pane startup variables to set. and other variables
13) configure --> sequence of steps --> prompts == error
13) configure --> application launchers --> Launch org.gnome.Ptyxis.desktop == doesnt launch terminal (check again )
14) shuold show in agents/ prompts pane all the saved prompts in config folder . should be able to create, edit, delete, read.
15) if a pad is already use, then warn me so i know that i am replacing it. 
11) 
99) Merge start wisprflow and repair
merge start wisprflow and repair where :
if wisprflow running :
repaird
else: (means not running)
start

