from manim import Circle, Scene, Square, VGroup


class Regroup(Scene):
    def construct(self):
        a = Square()
        b = Circle()
        group = VGroup(a, b)
        self.add(group)
        self.remove(a)
        self.add(group)
