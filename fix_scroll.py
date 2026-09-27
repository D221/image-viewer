import sys
with open(r'src\main.rs', encoding='utf-8') as f:
    lines = f.readlines()

# 1) Add scroll_to_current field to MyApp (next to waiting_load field decl)
w = next(i for i, l in enumerate(lines) if l.strip().startswith('waiting_load: bool,'))
lines.insert(w + 1, '    scroll_to_current: bool, // grid: scroll to the current image on next render\n')

# 2) Insert `scroll_to_current: false,` in the MyApp init (after waiting_load init)
i0 = next(i for i, l in enumerate(lines) if l.strip() == 'waiting_load: false,' and i > 600)
lines.insert(i0 + 1, '            scroll_to_current: false,\n')

# 3) Set the flag when toggling G into the grid (scroll there on first render)
for i, l in enumerate(lines):
    if l.strip() == 'if grid {' and 'gallery_view = !self.gallery_view' in lines[i + 1]:
        lines[i + 2:i + 2] = ['            self.scroll_to_current = true;\n']
        break

with open(r'src\main.rs', 'w', encoding='utf-8', newline='') as f:
    f.writelines(lines)
print('field + init + scroll flag done')