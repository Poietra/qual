import manim as mn


class Demo(mn.Scene):
    def construct(self):
        group = mn.AnimationGroup()
        self.play(group)
