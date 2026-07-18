import manim as mn


def push_right(mob):
    mob.shift(mn.RIGHT)


class Demo(mn.Scene):
    def construct(self):
        square = mn.Square()
        self.add(square)
        self.play(mn.ApplyFunction(push_right, square))
