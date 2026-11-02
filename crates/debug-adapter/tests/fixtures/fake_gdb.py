#!/usr/bin/env python3
"""Deterministic MI peer: no probe, network, ptrace, or installed debugger required."""
import re
import sys

state = "stopped"
frame = 0
breakpoints = False

def send(text):
    print(text, flush=True)

def stopped():
    send('*stopped,reason="breakpoint-hit",frame={level="0",func="compute",fullname="/fixture/main.c",line="8"},thread-id="1",stopped-threads="all"')

send('(gdb)')
for line in sys.stdin:
    match = re.match(r'(\d+)(.*)', line.rstrip('\n'))
    if not match:
        continue
    token, command = match.groups()
    if command == '-test-timeout':
        continue
    if command == '-test-exit':
        sys.exit(7)
    if command.startswith('-target-select'):
        send(token + '^connected')
        stopped()
    elif command.startswith('-exec-continue'):
        state = "running"
        send(token + '^running')
        send('*running,thread-id="all"')
    elif command.startswith(('-exec-interrupt', '-exec-step', '-exec-next', '-exec-finish')):
        state = "stopped"
        send(token + '^done')
        stopped()
    elif command == '-thread-info':
        send(token + '^done,threads=[{id="1",state="' + state + '"}],current-thread-id="1"')
    elif command.startswith('-stack-list-frames'):
        send(token + '^done,stack=[frame={level="0",func="compute",fullname="/fixture/main.c",line="8",addr="0x8000010"},frame={level="1",func="main",fullname="/fixture/main.c",line="20",addr="0x8000000"}]')
    elif command.startswith('-stack-select-frame'):
        frame = int(command.split()[-1])
        send(token + '^done')
    elif command.startswith('-break-insert'):
        breakpoints = True
        send(token + '^done,bkpt={number="1",enabled="y",original-location="compute"}')
    elif command.startswith('-break-delete'):
        breakpoints = False
        send(token + '^done')
    elif command == '-break-list':
        body = 'bkpt={number="1",enabled="y",original-location="compute"}' if breakpoints else ''
        send(token + '^done,BreakpointTable={body=[' + body + ']}')
    elif command.startswith('-var-create'):
        if 'missing' in command:
            send(token + '^error,msg="No symbol missing in current context"')
        else:
            send('98765^done,value="unrelated result"')
            send(token + '^done,name="var1",numchild="0",type="int",value="' + ('42' if frame == 0 else '7') + '"')
    elif command.startswith('-interpreter-exec'):
        state = "stopped"
        send(token + '^done')
        stopped()
    else:
        send(token + '^done')
    send('(gdb)')
