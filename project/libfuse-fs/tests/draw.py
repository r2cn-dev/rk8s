import graphviz

# 使用 Graphviz 绘制 fts_read 的主要控制流程图（简化核心路径）
dot = graphviz.Digraph(format='png')
dot.attr(rankdir='TB', size='10')

# 添加主要节点
dot.node('start', 'Start')
dot.node('nullcheck', 'fts_cur == NULL or FTS_STOP?')
dot.node('instr_check', 'p->fts_instr == FTS_AGAIN?')
dot.node('follow_check', 'p->fts_instr == FTS_FOLLOW &&\n(FTS_SL or FTS_SLNONE)?')
dot.node('pre_dir', 'p->fts_info == FTS_D?')
dot.node('skip_or_xdev', 'instr == FTS_SKIP or XDEV crossed?')
dot.node('nameonly', 'sp->fts_child != NULL && FTS_NAMEONLY?')
dot.node('child_exists', 'sp->fts_child != NULL?')
dot.node('build_fail', 'fts_build failed?')
dot.node('set_child', 'Set sp->fts_child')
dot.node('next_node', 'p->fts_link == NULL && has_dirp?')
dot.node('build_next', 'fts_build (next batch)')
dot.node('p_link', 'p = p->fts_link')
dot.node('root_level', 'p->fts_level == FTS_ROOTLEVEL?')
dot.node('restore_root', 'restore_initial_cwd()')
dot.node('skip_follow', 'p->fts_instr == FTS_SKIP?\nOr FTS_FOLLOW?')
dot.node('check_dir', 'p->fts_info == FTS_D?\nenter_dir()')
dot.node('return_p', 'return p')
dot.node('cd_dotdot', 'cd_dot_dot: p = p->fts_parent')
dot.node('is_rootparent', 'p->fts_level == FTS_ROOTPARENTLEVEL?')
dot.node('restore_dir', 'restore_initial_cwd or chdir("..")')
dot.node('return_dp', 'return (FTS_DP or NULL)')

# 添加边
dot.edges([
    ('start', 'nullcheck'),
    ('nullcheck', 'instr_check', {'label': 'No'}),
    ('nullcheck', 'return_p', {'label': 'Yes'}),
    ('instr_check', 'return_p', {'label': 'Yes'}),
    ('instr_check', 'follow_check', {'label': 'No'}),
    ('follow_check', 'check_dir', {'label': 'Yes'}),
    ('follow_check', 'pre_dir', {'label': 'No'}),
    ('pre_dir', 'skip_or_xdev', {'label': 'Yes'}),
    ('skip_or_xdev', 'return_p', {'label': 'Yes'}),
    ('skip_or_xdev', 'nameonly', {'label': 'No'}),
    ('nameonly', 'child_exists', {'label': 'Yes'}),
    ('child_exists', 'build_fail', {'label': 'No'}),
    ('build_fail', 'return_p', {'label': 'Yes'}),
    ('build_fail', 'set_child', {'label': 'No'}),
    ('set_child', 'return_p'),
    ('pre_dir', 'next_node', {'label': 'No'}),
    ('next_node', 'build_next', {'label': 'Yes'}),
    ('build_next', 'cd_dotdot', {'label': 'Fail'}),
    ('build_next', 'set_child', {'label': 'Success'}),
    ('next_node', 'p_link', {'label': 'No'}),
    ('p_link', 'root_level'),
    ('root_level', 'restore_root', {'label': 'Yes'}),
    ('restore_root', 'check_dir'),
    ('root_level', 'skip_follow', {'label': 'No'}),
    ('skip_follow', 'check_dir'),
    ('check_dir', 'return_p', {'label': 'Yes'}),
    ('cd_dotdot', 'is_rootparent'),
    ('is_rootparent', 'return_p', {'label': 'Yes'}),
    ('is_rootparent', 'restore_dir', {'label': 'No'}),
    ('restore_dir', 'return_dp')
])

dot.render('/mnt/data/fts_read_flowchart', cleanup=False)
'/mnt/data/fts_read_flowchart.png'
