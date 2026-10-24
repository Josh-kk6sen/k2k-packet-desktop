import os
os.chdir('/home/joshcarz/projects/packetterm-desktop/src-tauri')

with open('commands.rs', 'r') as f:
    content = f.read()

old = '                _ => "S",'
new = '''                _ => {
                    // Parse the specific control frame type from AGW header (e.g. "ctl rr" -> "RR")
                    let agw_header = String::from_utf8_lossy(&frame.data);
                    let first_line = agw_header.lines().next().unwrap_or("");
                    if let Some(ctl_pos) = first_line.find("ctl ") {
                        let rest = &first_line[ctl_pos + 4..];
                        let ctl_type = rest.split_whitespace().next().unwrap_or("S");
                        ctl_type.to_uppercase()
                    } else {
                        "S".to_string()
                    }
                }'''

idx = content.find('RSP_INFO_MON => "I"')
if idx >= 0:
    next_match = content.find('_ => "S"', idx)
    if next_match >= 0:
        context = content[next_match-20:next_match+20]
        print(f"Replacing at {next_match}: {repr(context)}")
        content = content[:next_match] + new + content[next_match + len(old):]
        with open('commands.rs', 'w') as f:
            f.write(content)
        print("DONE")
    else:
        print("NOT FOUND after RSP_INFO_MON")
else:
    print("RSP_INFO_MON not found")
