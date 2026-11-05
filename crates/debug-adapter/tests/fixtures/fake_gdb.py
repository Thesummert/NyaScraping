#!/usr/bin/env python3
"""Deterministic MI peer: no probe, network, ptrace, or installed debugger required."""
import re
import sys

state = "stopped"
frame = 0
breakpoints = False
breakpoint_enabled = True
breakpoint_source = False
values = {}
current_expression = ""
break_on_interrupt = False
live_counter = 0
fail_continue = False
memory_reads = 0
break_on_memory = False

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
    if command == '-test-fail-continue':
        fail_continue = True
        send(token + '^done')
        continue
    if command == '-test-break-on-interrupt':
        break_on_interrupt = True
        send(token + '^done')
        continue
    if command == '-test-break-on-memory':
        break_on_memory = True
        send(token + '^done')
        continue
    if command == '-test-timeout':
        continue
    if command == '-test-exit':
        sys.exit(7)
    if command == '-target-download':
        send('+download,{section=".text",section-size="1024",total-size="1024"}')
        send('+download,{section=".text",section-sent="512",section-size="1024",total-sent="512",total-size="1024"}')
        send(token + '^done,address="0x8000000",load-size="1024"')
    elif command.startswith('-target-select'):
        send(token + '^connected')
        stopped()
    elif command.startswith('-exec-continue'):
        if fail_continue:
            send(token + '^error,msg="Continue failed"')
            continue
        live_counter += 1
        state = "running"
        send(token + '^running')
        send('*running,thread-id="all"')
    elif command.startswith(('-exec-interrupt', '-exec-step', '-exec-next', '-exec-finish')):
        state = "stopped"
        send(token + '^done')
        if command.startswith('-exec-interrupt') and not break_on_interrupt:
            send('*stopped,reason="signal-received",signal-name="SIGINT",thread-id="1",stopped-threads="all"')
        else:
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
        breakpoint_enabled = True
        breakpoint_source = command.startswith('-break-insert --source "/fixture/main.c" --line 8')
        send(token + '^done,bkpt={number="1",enabled="y",original-location="compute"}')
    elif command.startswith('-break-enable'):
        breakpoint_enabled = True
        send(token + '^done')
    elif command.startswith('-break-disable'):
        breakpoint_enabled = False
        send(token + '^done')
    elif command.startswith('-break-delete'):
        breakpoints = False
        send(token + '^done')
    elif command == '-break-list':
        body = ('bkpt={number="1",enabled="' + ('y' if breakpoint_enabled else 'n') + '",original-location="compute"' + (',fullname="/fixture/main.c",line="8"' if breakpoint_source else '') + '}') if breakpoints else ''
        send(token + '^done,BreakpointTable={body=[' + body + ']}')
    elif command.startswith('-var-show-attributes'):
        editable = current_expression not in ('2 + 3', 'readonly')
        send(token + '^done,attr="' + ('editable' if editable else 'noneditable') + '"')
    elif command.startswith('-var-assign'):
        value = command.split(' ', 2)[2].strip('"')
        if value == 'invalid':
            send(token + '^error,msg="Invalid value"')
        else:
            values[current_expression] = value
            send(token + '^done,value="' + value + '"')
    elif command.startswith('-data-read-memory-bytes'):
        _, address, size = command.split()
        address, size = int(address, 0), int(size)
        if address == 0xDEADBEEF:
            send(token + '^error,msg="Memory access failed"')
        else:
            memory_reads += 1
            if break_on_memory:
                state = "stopped"
                stopped()
                break_on_memory = False
            data = bytes((memory_reads + i) & 255 for i in range(size)).hex()
            send(token + '^done,memory=[{begin="' + hex(address) + '",offset="0",end="' + hex(address + size) + '",contents="' + data + '"}]')
    elif command.startswith('-data-evaluate-expression'):
        expression = command.split(' ', 1)[1].strip('"')
        send(token + '^done,value="' + values.get(expression, '42') + '"')
    elif command.startswith('-var-create'):
        current_expression = command.split(' ', 3)[3].strip('"')
        if 'missing' in command:
            send(token + '^error,msg="No symbol missing in current context"')
        else:
            send('98765^done,value="unrelated result"')
            send(token + '^done,name="var1",numchild="0",type="int",value="' + values.get(current_expression, str(live_counter) if current_expression == 'live_counter' else ('42' if frame == 0 else '7')) + '"')
    elif command.startswith('-interpreter-exec'):
        state = "stopped"
        send(token + '^done')
        stopped()
    else:
        send(token + '^done')
    send('(gdb)')
