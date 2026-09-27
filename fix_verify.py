with open(r'src\main.rs', encoding='utf-8') as f:
    lines = f.readlines()
# append grid_cell method after the inserted gallery_grid (verify first)
marker = next(i for i, l in enumerate(lines) if 'Per-frame housekeeping' in l)
print('marker', marker + 1, 'grid present:', any('fn gallery_grid' in l for l in lines))
