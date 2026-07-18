from manim import *


class SafePath(VMobject):
    def flat_points(self):
        # A flat (N, 3) reshape carries no per-curve assumption.
        return self.points.reshape((-1, 3))

    def every_other(self):
        # Stride 2 makes no layout claim.
        return self.points[::2]

    def helper(self, mob):
        # Not `self`: the receiver's class is unknown.
        return mob.points.reshape((-1, 4, 3))


class NotAMobject:
    def slices(self):
        # The class is no VMobject subclass.
        return self.points[0::4]
