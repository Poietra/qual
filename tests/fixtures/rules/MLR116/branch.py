from manim import *


class MaybeStartedScene(Scene):
    def construct(self):
        path = VMobject()
        if unknown_condition():
            path.start_new_path(ORIGIN)
        # The path is only Maybe-empty here: the rule must stay silent.
        path.add_line_to([1, 0, 0])
        self.add(path)
