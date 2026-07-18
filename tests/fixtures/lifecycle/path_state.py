from manim import *


class PathState(Scene):
    def construct(self):
        path = VMobject()
        path.start_new_path(LEFT)
        path.add_line_to(RIGHT)
        self.add(path)
        sq = Square()
        sq.add_line_to(RIGHT)


class EmptyPathBug(Scene):
    def construct(self):
        bug = VMobject()
        bug.add_line_to(RIGHT)


class WidenedPath(Scene):
    def construct(self):
        fuzzy = VMobject()
        fuzzy.mystery()
        fuzzy.add_line_to(RIGHT)
